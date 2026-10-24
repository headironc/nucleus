use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use chrono::{DateTime, Utc};
use platform::database::Session;
use tokio::{
    sync::Mutex,
    time::{Instant, timeout_at},
};

use crate::runtime::{Error, Sample};

pub(crate) struct Clock {
    session: Mutex<Session>,
    failed: AtomicBool,
}

impl Clock {
    pub(crate) fn new(session: Session) -> Self {
        Self {
            session: Mutex::new(session),
            failed: AtomicBool::new(false),
        }
    }

    pub(crate) async fn sample(&self) -> Result<Sample, Error> {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut session = timeout_at(deadline, self.session.lock()).await?;
        // Cancellation leaves sampling closed until the owner finishes stopping the driver.
        if self.failed.swap(true, Ordering::AcqRel) {
            return Err(Error::ClockClosed.combine(session.stop().await));
        }
        let sampling = Sampling {
            session: &mut session,
            failed: &self.failed,
        };
        let error = match timeout_at(deadline, sample(sampling.session)).await {
            Ok(Ok(now)) => {
                self.failed.store(false, Ordering::Release);
                return Ok(now);
            }
            Ok(Err(error)) => error,
            Err(error) => error.into(),
        };
        Err(error.combine(sampling.session.stop().await))
    }

    pub(crate) async fn stop(&self) -> Result<(), Error> {
        let mut session = self.session.lock().await;
        self.failed.store(true, Ordering::Release);
        session.stop().await?;
        Ok(())
    }
}

// Cancel the driver on an interrupted sample; retain its handle to await termination.
struct Sampling<'a> {
    session: &'a mut Session,
    failed: &'a AtomicBool,
}

impl Drop for Sampling<'_> {
    fn drop(&mut self) {
        if self.failed.load(Ordering::Acquire) {
            self.session.abort();
        }
    }
}

async fn sample(session: &Session) -> Result<Sample, Error> {
    let client = session.client();
    client
        .batch_execute("BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL statement_timeout = '3s'")
        .await?;
    let started_at = Instant::now();
    let observed_at: DateTime<Utc> = client
        .query_one("SELECT clock_timestamp()", &[])
        .await?
        .try_get(0)?;
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
    Ok(Sample {
        observed_at,
        started_at,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use platform::database::{Connections, credentials::Password, settings};
    use rustls::RootCertStore;
    use tokio::{runtime::Runtime, time::timeout};

    use crate::runtime::{Error, clock::Clock};

    #[test]
    fn cancelled_sampling_cannot_reuse_or_replace_its_session() {
        let server = fixture::Server::start();
        Runtime::new().expect("test runtime").block_on(async {
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut roots = RootCertStore::empty();
            roots.add(proxy.authority()).expect("proxy trust anchor");
            let connections = Connections::new(
                settings::Settings::new(
                    "127.0.0.1".into(),
                    proxy.port(),
                    "postgres".into(),
                    "postgres".into(),
                    2,
                    settings::Budgets::default(),
                )
                .expect("fixture settings"),
                Password::new(fixture::PASSWORD.into()).expect("fixture password"),
                roots,
            )
            .expect("fixture connections");
            let clock = Arc::new(Clock::new(
                connections.dedicated().await.expect("dedicated session"),
            ));
            // Hold the time query before any persistent clock table is accessed.
            proxy.arm(fixture::database_proxy::Fault::StalledTime);
            let worker = Arc::clone(&clock);
            let task = tokio::spawn(async move { worker.sample().await });
            timeout(Duration::from_secs(2), proxy.intercepted())
                .await
                .expect("time query intercepted");
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
            assert!(matches!(clock.sample().await, Err(Error::ClockClosed)));
            clock.stop().await.expect("stop failed clock");
            clock.stop().await.expect("repeat stop");
            assert!(matches!(clock.sample().await, Err(Error::ClockClosed)));
            connections.close().await;
            proxy.stop().await;
        });
    }
}
