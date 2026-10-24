//! Transaction time ordered against the process high-water mark.

use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use tokio::{
    sync::{Mutex, watch},
    time::Instant,
};
use tokio_postgres::GenericClient;

use crate::runtime::Error;

pub(crate) struct Time {
    observed_at: Mutex<Option<DateTime<Utc>>>,
    closed: AtomicBool,
    admission: watch::Sender<bool>,
}

impl Time {
    pub(crate) fn new(admission: watch::Sender<bool>) -> Self {
        Self {
            observed_at: Mutex::new(None),
            closed: AtomicBool::new(false),
            admission,
        }
    }

    /// Keep the query and comparison in one order, including concurrent slow responses.
    pub(crate) async fn observe<C: GenericClient + Sync>(
        &self,
        client: &C,
    ) -> Result<Sample, Error> {
        let mut high_water = self.observed_at.lock().await;
        self.ready()?;
        let started_at = Instant::now();
        let row = client.query_opt(
            if high_water.is_some() {
                "SELECT clock_timestamp() AS observed_at"
            } else {
                "SELECT clock_timestamp() AS observed_at, observed_at AS persisted_at FROM security.clocks WHERE id"
            },
            &[],
        ).await?;
        let Some(row) = row else {
            self.close();
            return Err(Error::ClockInvalid);
        };
        let observed_at: DateTime<Utc> = row.try_get("observed_at")?;
        let previous = match *high_water {
            Some(previous) => previous,
            None => row.try_get("persisted_at")?,
        };
        if observed_at < previous {
            self.close();
            return Err(Error::ClockInvalid);
        }
        self.ready()?;
        *high_water = Some(observed_at);
        Ok(Sample {
            observed_at,
            started_at,
        })
    }

    /// Persistence copies the latest observation without holding up business transactions.
    pub(crate) async fn high_water(&self) -> Option<DateTime<Utc>> {
        *self.observed_at.lock().await
    }

    // Closing new-request admission still permits already accepted work to finish.
    pub(crate) fn ready(&self) -> Result<(), Error> {
        if self.closed.load(Ordering::Acquire) {
            Err(Error::ClockClosed)
        } else {
            Ok(())
        }
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.admission.send_replace(true);
    }
}

/// The monotonic origin belongs to the same database observation as its wall-clock time.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sample {
    observed_at: DateTime<Utc>,
    started_at: Instant,
}

impl Sample {
    pub(crate) fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }

    pub(crate) fn started_at(&self) -> Instant {
        self.started_at
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{future::Future, sync::Arc, task::Poll, time::Duration};

    use chrono::{DateTime, Utc};
    use fixture::database_proxy::{Fault, Proxy};
    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        identity::authentication::{Database, proxied, server},
        runtime::{Context, Error},
    };

    #[test]
    fn rollback_keeps_the_process_high_water_without_a_clock_write() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(10), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let context = Context::new(database.connections(&database.user))
                    .await
                    .expect("context");
                let writer = context.connections().recover().await.expect("writer");
                let observer = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("observer");
                let before: DateTime<Utc> = observer
                    .client()
                    .query_one("SELECT observed_at FROM security.clocks WHERE id", &[])
                    .await
                    .expect("persisted floor")
                    .get(0);
                let instance = context.clock_session().await;
                writer.client().batch_execute("BEGIN").await.expect("begin");
                let now = context
                    .now(writer.client())
                    .await
                    .expect("transaction time");
                writer
                    .client()
                    .batch_execute("ROLLBACK")
                    .await
                    .expect("rollback");
                assert!(now >= before);
                // The same observer sees no persistent write, even after the business end.
                let after: DateTime<Utc> = observer
                    .client()
                    .query_one("SELECT observed_at FROM security.clocks WHERE id", &[])
                    .await
                    .expect("unchanged persisted floor")
                    .get(0);
                assert_eq!(before, after);
                drop(instance);
                writer.terminate().await.expect("writer ended");
                context.stop().await.expect("persist and stop");
                let persisted: DateTime<Utc> = observer
                    .client()
                    .query_one("SELECT observed_at FROM security.clocks WHERE id", &[])
                    .await
                    .expect("persisted process high water")
                    .get(0);
                assert_eq!(persisted, now, "rollback did not undo the observation");
                observer.terminate().await.expect("observer ended");
            })
            .await
            .expect("bounded rollback and persistence");
        });
    }

    #[test]
    fn overlapping_observations_are_serialized_and_real_rewinds_close_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                for rewind in [false, true] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let context = Context::new(proxied(&database, &proxy))
                        .await
                        .expect("context");
                    context.sample().await.expect("seed high water");
                    let first = context
                        .connections()
                        .recover()
                        .await
                        .expect("first transaction");
                    let second = context
                        .connections()
                        .lease()
                        .await
                        .expect("second transaction");
                    first
                        .client()
                        .batch_execute("BEGIN")
                        .await
                        .expect("first begin");
                    second
                        .client()
                        .batch_execute("BEGIN")
                        .await
                        .expect("second begin");
                    let before = proxy.time_queries();
                    proxy.arm(Fault::StalledTime);
                    let concurrent = context.clone();
                    let original = tokio::spawn(async move {
                        let now = concurrent.now(first.client()).await;
                        first
                            .client()
                            .batch_execute("ROLLBACK")
                            .await
                            .expect("first rollback");
                        first.terminate().await.expect("first ended");
                        now
                    });
                    timeout(Duration::from_secs(2), proxy.intercepted())
                        .await
                        .expect("first observation entered");
                    let mut waiting = Box::pin(context.now(second.client()));
                    std::future::poll_fn(|task| {
                        assert!(waiting.as_mut().poll(task).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                    assert_eq!(
                        proxy.time_queries() - before,
                        1,
                        "the second query waits before sending"
                    );
                    if rewind {
                        proxy.arm(Fault::RewoundCurrent);
                    }
                    proxy.release();
                    let previous = original
                        .await
                        .expect("first task")
                        .expect("first observation");
                    if rewind {
                        std::future::poll_fn(|task| {
                            assert!(waiting.as_mut().poll(task).is_pending());
                            Poll::Ready(())
                        })
                        .await;
                        timeout(Duration::from_secs(2), proxy.intercepted())
                            .await
                            .expect("second observation rewound");
                        proxy.release();
                        assert!(matches!(waiting.await, Err(Error::ClockInvalid)));
                        assert!(*context.admission().borrow());
                    } else {
                        assert!(waiting.await.expect("second observation") >= previous);
                        assert!(!*context.admission().borrow());
                    }
                    second
                        .client()
                        .batch_execute("ROLLBACK")
                        .await
                        .expect("second rollback");
                    second.release();
                    context.stop().await.expect("stop");
                    proxy.stop().await;
                }
            })
            .await
            .expect("bounded concurrent observation");
        });
    }

    #[test]
    fn a_restarted_process_rejects_time_below_its_persisted_high_water() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(10), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let context = Context::new(database.connections(&database.user))
                    .await
                    .expect("first context");
                context.sample().await.expect("observed time");
                context.stop().await.expect("persist before stop");
                let proxy = Proxy::start(&server).await;
                let restarted = Context::new(proxied(&database, &proxy))
                    .await
                    .expect("restarted context");
                proxy.arm(Fault::RewoundCurrent);
                let copied = restarted.clone();
                let observation = tokio::spawn(async move { copied.sample().await });
                timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("rewound database time");
                proxy.release();
                assert!(matches!(
                    observation.await.expect("task"),
                    Err(Error::ClockInvalid)
                ));
                assert!(*restarted.admission().borrow());
                restarted.stop().await.expect("stop rejected context");
                proxy.stop().await;
            })
            .await
            .expect("bounded restart");
        });
    }
}
