//! Shared trusted time and database connections. Business checks remain with their consumers.

mod clock;
mod error;

use std::{mem, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use platform::database::Connections;
use tokio::{
    sync::Mutex,
    time::{Instant, timeout},
};

use crate::runtime::clock::Clock;
pub use crate::runtime::error::Error;

/// Clones share runtime resources; the application stops them after all consumers drain.
#[derive(Clone)]
pub struct Context(Arc<Inner>);

impl Context {
    pub async fn new(connections: Connections) -> Result<Self, Error> {
        let error = match timeout(Duration::from_secs(3), connections.dedicated()).await {
            Ok(Ok(session)) => {
                return Ok(Self(Arc::new(Inner {
                    connections,
                    clock: Clock::new(session),
                    termination: Mutex::new(Ok(())),
                })));
            }
            Ok(Err(error)) => error.into(),
            Err(error) => error.into(),
        };
        connections.close().await;
        Err(error)
    }

    pub(crate) async fn sample(&self) -> Result<DateTime<Utc>, Error> {
        Ok(self.observe().await?.observed_at())
    }

    pub(crate) async fn observe(&self) -> Result<Sample, Error> {
        self.0.clock.sample().await
    }

    pub(crate) fn connections(&self) -> &Connections {
        &self.0.connections
    }

    /// The application must drain every consumer before closing shared resources.
    /// Cancelled waits retain the clock result until all connections have ended.
    pub async fn stop(&self) -> Result<(), Error> {
        let mut result = self.0.termination.lock().await;
        if result.is_ok() {
            *result = self.0.clock.stop().await;
        }
        self.0.connections.close().await;
        mem::replace(&mut *result, Ok(()))
    }
}

struct Inner {
    connections: Connections,
    clock: Clock,
    termination: Mutex<Result<(), Error>>,
}

/// The monotonic origin is captured immediately before the database time query.
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

    use platform::database::{self, Connections, credentials::Password, settings};
    use rustls::RootCertStore;
    use tokio::{runtime::Builder, sync::Mutex, time::timeout};

    use crate::runtime::{Context, Error, Inner, clock::Clock};

    fn connections(server: &fixture::Server) -> Connections {
        let mut roots = RootCertStore::empty();
        roots
            .add(server.certificate().clone())
            .expect("fixture trust anchor");
        Connections::new(
            settings::Settings::new(
                "127.0.0.1".into(),
                server.port(),
                "postgres".into(),
                "postgres".into(),
                2,
                settings::Budgets::default(),
            )
            .expect("fixture settings"),
            Password::new(fixture::PASSWORD.into()).expect("fixture password"),
            roots,
        )
        .expect("fixture connections")
    }

    #[test]
    fn cancelled_stop_preserves_the_clock_failure_until_connections_end() {
        let server = fixture::Server::start();
        Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(async {
                timeout(Duration::from_secs(10), async {
                    let connections = connections(&server);
                    let session = connections.dedicated().await.expect("clock session");
                    let pid: i32 = session
                        .client()
                        .query_one("SELECT pg_backend_pid()", &[])
                        .await
                        .expect("clock backend")
                        .get(0);
                    let admin = connections.dedicated().await.expect("fixture admin");
                    let ended: bool = admin
                        .client()
                        .query_one("SELECT pg_terminate_backend($1)", &[&pid])
                        .await
                        .expect("end fixture clock backend")
                        .get(0);
                    assert!(ended);
                    while !session.client().is_closed() {
                        tokio::task::yield_now().await;
                    }
                    let context = Context(Arc::new(Inner {
                        connections,
                        clock: Clock::new(session),
                        termination: Mutex::new(Ok(())),
                    }));
                    let shared = context.clone();
                    {
                        let mut stop = std::pin::pin!(context.stop());
                        // The failed clock driver is finished. The live admin driver cannot
                        // process its abort until this single-threaded runtime yields.
                        std::future::poll_fn(|task| {
                            assert!(stop.as_mut().poll(task).is_pending());
                            Poll::Ready(())
                        })
                        .await;
                        assert!(
                            !admin.client().is_closed(),
                            "another driver still needs to end"
                        );
                        let mut concurrent = std::pin::pin!(shared.stop());
                        std::future::poll_fn(|task| {
                            assert!(concurrent.as_mut().poll(task).is_pending());
                            Poll::Ready(())
                        })
                        .await;
                    }
                    let error = shared
                        .stop()
                        .await
                        .expect_err("report the original clock failure after cancelled waits");
                    assert!(
                        matches!(error, Error::Connection(database::Error::Postgres(ref cause))
                    if cause.code() == Some(&tokio_postgres::error::SqlState::ADMIN_SHUTDOWN))
                    );
                    assert!(
                        admin.client().is_closed(),
                        "all drivers ended before the error is returned"
                    );
                    assert!(
                        context.connections().acquire().await.is_err(),
                        "online admission stays closed"
                    );
                    assert!(
                        context.connections().recover().await.is_err(),
                        "recovery admission stays closed"
                    );
                    assert!(matches!(context.sample().await, Err(Error::ClockClosed)));
                    context.stop().await.expect("failure is reported only once");
                    admin
                        .terminate()
                        .await
                        .expect("join the cancelled fixture admin");
                })
                .await
                .expect("bounded cancellation regression");
            });
    }

    #[test]
    fn normal_stop_is_shared_and_repeatable() {
        let server = fixture::Server::start();
        Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(async {
                timeout(Duration::from_secs(10), async {
                    let context = Context::new(connections(&server))
                        .await
                        .expect("runtime context");
                    let session = context
                        .connections()
                        .dedicated()
                        .await
                        .expect("online consumer");
                    let shared = context.clone();
                    context.stop().await.expect("normal stop");
                    assert!(session.client().is_closed());
                    assert!(matches!(shared.sample().await, Err(Error::ClockClosed)));
                    shared.stop().await.expect("stop through another handle");
                    session
                        .terminate()
                        .await
                        .expect("join the stopped consumer");
                })
                .await
                .expect("bounded normal stop");
            });
    }
}
