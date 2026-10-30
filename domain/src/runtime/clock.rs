use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use platform::database::Session;
use tokio::{
    sync::{Mutex, MutexGuard},
    time::{Instant, timeout_at},
};

use crate::{kernel::time::Time, runtime::Error};

pub(super) const BUDGET: Duration = Duration::from_secs(3);

/// The trusted-time session; in the application it is also the instance session.
/// Any failure ends the session, which releases the instance lock, so admission closes.
pub(crate) struct Clock {
    session: Mutex<Session>,
    time: Arc<Time>,
    failed: AtomicBool,
}

impl Clock {
    pub(crate) fn new(session: Session, time: Arc<Time>) -> Self {
        Self {
            session: Mutex::new(session),
            time,
            failed: AtomicBool::new(false),
        }
    }

    pub(crate) async fn persist(&self, deadline: Instant) -> Result<(), Error> {
        let session = match timeout_at(deadline, self.session.lock()).await {
            Ok(session) => session,
            Err(error) => {
                self.close();
                return Err(error.into());
            }
        };
        self.run(session, deadline, async |session| {
            if let Some(observed_at) = self.time.high_water().await {
                persist(session, observed_at).await?;
            }
            Ok(())
        })
        .await
    }

    /// Confirm that this session still holds the instance lock.
    /// Persistence is bounded, so the heartbeat queues fairly and has its own budget.
    pub(crate) async fn heartbeat(&self) -> Result<(), Error> {
        let session = self.session.lock().await;
        self.run(session, Instant::now() + BUDGET, held).await
    }

    async fn run<T>(
        &self,
        mut session: MutexGuard<'_, Session>,
        deadline: Instant,
        operation: impl AsyncFnOnce(&Session) -> Result<T, Error>,
    ) -> Result<T, Error> {
        // Interrupted maintenance closes admission until the owner has stopped the driver.
        if self.failed.swap(true, Ordering::AcqRel) {
            self.close();
            return Err(Error::ClockClosed.combine(session.stop().await));
        }
        let running = Running {
            session: &mut session,
            failed: &self.failed,
            time: &self.time,
        };
        let error = match timeout_at(deadline, operation(running.session)).await {
            Ok(Ok(value)) => {
                self.failed.store(false, Ordering::Release);
                return Ok(value);
            }
            Ok(Err(error)) => error,
            Err(error) => error.into(),
        };
        self.close();
        Err(error.combine(running.session.stop().await))
    }

    #[cfg(test)]
    pub(crate) async fn session(&self) -> MutexGuard<'_, Session> {
        self.session.lock().await
    }

    /// The driver ended or the session was lost; nothing reopens it.
    pub(crate) fn close(&self) {
        self.failed.store(true, Ordering::Release);
        self.time.close();
    }

    /// The maintenance owner finishes persistence before releasing the instance session.
    pub(crate) async fn stop(&self, instance: bool) -> Result<(), platform::database::Error> {
        let mut session = self.session.lock().await;
        if instance && !self.failed.load(Ordering::Acquire) {
            crate::database::instance::unlock(&session).await;
        }
        self.failed.store(true, Ordering::Release);
        self.time.close();
        session.stop().await
    }
}

// Cancel the driver on interrupted maintenance; retain its handle to await termination.
struct Running<'a> {
    session: &'a mut Session,
    failed: &'a AtomicBool,
    time: &'a Time,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if self.failed.load(Ordering::Acquire) {
            self.time.close();
            self.session.abort();
        }
    }
}

async fn held(session: &Session) -> Result<(), Error> {
    let held: bool = session
        .client()
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks lock
                WHERE lock.locktype = 'advisory' AND lock.classid = 1314210636 AND lock.objid = 3
                    AND lock.objsubid = 2 AND lock.granted AND lock.pid = pg_catalog.pg_backend_pid()
                    AND lock.database = (SELECT database.oid FROM pg_catalog.pg_database database
                        WHERE database.datname = pg_catalog.current_database()))",
            &[],
        )
        .await?
        .try_get(0)?;
    if !held {
        return Err(Error::InstanceLost);
    }
    Ok(())
}

async fn persist(session: &Session, observed_at: DateTime<Utc>) -> Result<(), Error> {
    let client = session.client();
    client
        .batch_execute("BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL statement_timeout = '3s'")
        .await?;
    let updated = client
        .execute(
            "UPDATE security.clocks SET observed_at = $1 WHERE id AND observed_at <= $1",
            &[&observed_at],
        )
        .await?;
    if updated != 1 {
        return Err(Error::ClockInvalid);
    }
    client.batch_execute("COMMIT").await?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{future::Future, sync::Arc, task::Poll, time::Duration};

    use fixture::database_proxy::{Fault, Proxy};
    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        identity::authentication::{Database, proxied, server},
        runtime::{Context, Error},
    };

    #[test]
    fn cancelled_stop_still_persists_the_high_water_before_shutdown() {
        let server = server();
        Runtime::new().expect("test runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let context = Context::hold(proxied(&database, &proxy))
                    .await
                    .expect("instance context");
                let observer = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("independent observer");
                let before: chrono::DateTime<chrono::Utc> = observer
                    .client()
                    .query_one("SELECT observed_at FROM security.clocks WHERE id", &[])
                    .await
                    .expect("previous persisted time")
                    .get(0);
                let observed = context.sample().await.expect("transaction time");
                proxy.arm(Fault::StalledClock);
                let mut stopping = Box::pin(context.stop());
                std::future::poll_fn(|task| {
                    assert!(stopping.as_mut().poll(task).is_pending());
                    Poll::Ready(())
                })
                .await;
                drop(stopping);
                let pid = timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("owned persistence reached its write");
                assert!(
                    !*context.admission().borrow(),
                    "cancelled waits leave the actual persistence running"
                );
                proxy.release();
                context
                    .stop()
                    .await
                    .expect("drain the original persistence");
                let after: chrono::DateTime<chrono::Utc> = observer
                    .client()
                    .query_one("SELECT observed_at FROM security.clocks WHERE id", &[])
                    .await
                    .expect("independent committed time after shutdown")
                    .get(0);
                assert!(after > before);
                assert_eq!(
                    after, observed,
                    "the last observation is durable before driver end"
                );
                timeout(Duration::from_secs(2), async {
                    loop {
                        let alive: bool = observer
                            .client()
                            .query_one(
                                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",
                                &[&pid],
                            )
                            .await
                            .expect("original clock backend")
                            .get(0);
                        if !alive {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("original backend ended before fixture cleanup");
                context.stop().await.expect("repeat shutdown");
                observer.terminate().await.expect("observer ended");
                proxy.stop().await;
            })
            .await
            .expect("bounded cancelled wait and shutdown");
        });
    }

    #[test]
    fn cancelled_stop_retains_an_unknown_persistence_commit() {
        let server = server();
        Runtime::new().expect("test runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let context = Context::new(proxied(&database, &proxy))
                    .await
                    .expect("runtime context");
                let clock = Arc::clone(&context.0.clock);
                context.sample().await.expect("transaction time");
                proxy.arm(Fault::LostClockCommit);
                let mut waiting = Box::pin(context.stop());
                std::future::poll_fn(|task| {
                    assert!(waiting.as_mut().poll(task).is_pending());
                    Poll::Ready(())
                })
                .await;
                drop(waiting);
                timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("original clock commit acknowledgement was lost");
                proxy.release();
                timeout(Duration::from_secs(2), async {
                    loop {
                        if context
                            .0
                            .monitor
                            .lock()
                            .await
                            .as_ref()
                            .expect("owned maintenance")
                            .is_finished()
                        {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("persistence failure completed without a shutdown waiter");
                assert!(
                    *context.admission().borrow(),
                    "a persistence failure closes admission"
                );
                let session = clock.session.lock().await;
                let mut stopping = Box::pin(context.stop());
                std::future::poll_fn(|task| {
                    assert!(stopping.as_mut().poll(task).is_pending());
                    Poll::Ready(())
                })
                .await;
                drop(stopping);
                drop(session);
                let error = context
                    .stop()
                    .await
                    .expect_err("cancelled shutdown retains the original sampling failure");
                assert!(matches!(error, Error::Query(_) | Error::Combined { .. }));
                context.stop().await.expect("report the failure only once");
                proxy.stop().await;
            })
            .await
            .expect("bounded cancelled shutdown after sampling failure");
        });
    }

    #[test]
    fn cancelled_persistence_cannot_reuse_or_replace_its_session() {
        let server = server();
        Runtime::new().expect("test runtime").block_on(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let context = Context::new(proxied(&database, &proxy))
                .await
                .expect("context");
            context.sample().await.expect("transaction time");
            let clock = Arc::clone(&context.0.clock);
            let admission = context.admission();
            proxy.arm(fixture::database_proxy::Fault::StalledClock);
            let worker = Arc::clone(&clock);
            let task = tokio::spawn(async move {
                worker
                    .persist(tokio::time::Instant::now() + super::BUDGET)
                    .await
            });
            timeout(Duration::from_secs(2), proxy.intercepted())
                .await
                .expect("persistence write intercepted");
            task.abort();
            assert!(task.await.expect_err("sample cancelled").is_cancelled());
            timeout(Duration::from_secs(1), async {
                let session = clock.session.lock().await;
                while !session.client().is_closed() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("cancelled sample aborts the original driver");
            proxy.release();
            assert!(*admission.borrow(), "a cancelled sample closes admission");
            assert!(matches!(context.sample().await, Err(Error::ClockClosed)));
            assert!(
                context.stop().await.is_err(),
                "failed maintenance remains closed"
            );
            context.stop().await.expect("repeat stop");
            assert!(matches!(context.sample().await, Err(Error::ClockClosed)));
            proxy.stop().await;
        });
    }

    #[test]
    fn background_persistence_finishes_inside_the_window_or_closes_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                for lost in [false, true] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let context = Context::hold(proxied(&database, &proxy))
                        .await
                        .expect("context");
                    let now = context.sample().await.expect("observation");
                    let observer = database
                        .connections("postgres")
                        .recover()
                        .await
                        .expect("observer");
                    if lost {
                        proxy.arm(Fault::LostClockCommit);
                    }
                    timeout(Duration::from_secs(10), async {
                        if lost {
                            proxy.intercepted().await;
                            proxy.release();
                            let mut closing = context.admission().subscribe();
                            while !*closing.borrow_and_update() {
                                closing
                                    .changed()
                                    .await
                                    .expect("maintenance closes admission");
                            }
                        } else {
                            let mut interval = tokio::time::interval(Duration::from_millis(50));
                            loop {
                                interval.tick().await;
                                let persisted: chrono::DateTime<chrono::Utc> = observer
                                    .client()
                                    .query_one(
                                        "SELECT observed_at FROM security.clocks WHERE id",
                                        &[],
                                    )
                                    .await
                                    .expect("independent persistent high water")
                                    .get(0);
                                if persisted >= now {
                                    break;
                                }
                            }
                            assert!(!*context.admission().borrow());
                        }
                    })
                    .await
                    .expect("persistence or closure is bounded by ten seconds");
                    if lost {
                        assert!(
                            context.sample().await.is_err(),
                            "unknown persistence is never reused"
                        );
                        assert!(
                            context.stop().await.is_err(),
                            "the background failure remains reportable"
                        );
                    } else {
                        context.stop().await.expect("stop");
                    }
                    observer.terminate().await.expect("observer ended");
                    proxy.stop().await;
                }
            })
            .await
            .expect("bounded persistence scenarios");
        });
    }
}
