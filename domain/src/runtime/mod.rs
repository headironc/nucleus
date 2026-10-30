//! Shared transaction time, instance maintenance and database connections.

mod clock;
mod error;
pub(crate) mod registry;
pub mod work;

use std::{mem, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use platform::database::{Connections, Ended, Session};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior, interval_at, timeout},
};

pub use crate::runtime::error::Error;
use crate::{
    database::{instance::Instance, read},
    kernel::time::{Sample, Time},
    runtime::clock::Clock,
};

const HEARTBEAT: Duration = Duration::from_secs(5);
// Reserve the write budget inside the ten-second loss window, including heartbeat waits.
const PERSIST: Duration = Duration::from_secs(10).saturating_sub(clock::BUDGET);

/// Clones share runtime resources; the application stops them after all consumers drain.
#[derive(Clone)]
pub struct Context(Arc<Inner>);

impl Context {
    /// Background time persistence on a session that holds no instance lock.
    /// The caller owns process exclusion, as deployment commands and tests do.
    pub async fn new(connections: Connections) -> Result<Self, Error> {
        Self::build(connections, read::Budget::default()).await
    }

    /// The application's background persistence shares the instance session.
    ///
    /// The instance lock is refused when another application or deployment command
    /// holds it. Once held, a driver end or a failed five-second heartbeat closes
    /// admission; the lock is never retaken, and a new process verifies again.
    pub async fn hold(connections: Connections) -> Result<Self, Error> {
        let instance = match Instance::hold(&connections).await {
            Ok(instance) => instance,
            Err(error) => {
                connections.close().await;
                return Err(error);
            }
        };
        let (session, ended) = instance.into_parts();
        Ok(Self::assemble(
            connections,
            session,
            read::Budget::default(),
            Some(ended),
        ))
    }

    #[cfg(test)]
    pub(crate) async fn with_read_budget(
        connections: Connections,
        duration: Duration,
    ) -> Result<Self, Error> {
        Self::build(connections, read::Budget::new(duration)).await
    }

    async fn build(connections: Connections, read: read::Budget) -> Result<Self, Error> {
        let error = match timeout(Duration::from_secs(3), connections.dedicated()).await {
            Ok(Ok(session)) => return Ok(Self::assemble(connections, session, read, None)),
            Ok(Err(error)) => error.into(),
            Err(error) => error.into(),
        };
        connections.close().await;
        Err(error)
    }

    fn assemble(
        connections: Connections,
        session: Session,
        read: read::Budget,
        ended: Option<Ended>,
    ) -> Self {
        let admission = watch::channel(false).0;
        let time = Arc::new(Time::new(admission.clone()));
        let clock = Arc::new(Clock::new(session, Arc::clone(&time)));
        let instance = ended.is_some();
        let (stopping, stopped) = watch::channel(false);
        let monitor = tokio::spawn(monitor(Arc::clone(&clock), ended, stopped));
        Self(Arc::new(Inner {
            connections,
            clock,
            time,
            instance,
            read,
            termination: Mutex::new(Ok(())),
            admission,
            generation: std::sync::Mutex::new(None),
            stopping,
            monitor: Mutex::new(Some(monitor)),
        }))
    }

    pub(crate) async fn now<C: tokio_postgres::GenericClient + Sync>(
        &self,
        client: &C,
    ) -> Result<DateTime<Utc>, Error> {
        Ok(self.observe(client).await?.observed_at())
    }

    /// A time-only read uses its own implicit transaction, never the instance connection.
    pub(crate) async fn read_time(&self) -> Result<DateTime<Utc>, Error> {
        self.0.time.ready()?;
        let lease = self.connections().lease().await?;
        match self.now(lease.client()).await {
            Ok(now) => {
                lease.release();
                Ok(now)
            }
            Err(error) => Err(error.combine(lease.discard().await)),
        }
    }

    #[cfg(test)]
    pub(crate) async fn sample(&self) -> Result<DateTime<Utc>, Error> {
        self.read_time().await
    }

    /// Confirm on the instance session that this process still holds the instance lock.
    /// A failure ends the session and closes admission.
    pub(crate) async fn heartbeat(&self) -> Result<(), Error> {
        self.0.clock.heartbeat().await
    }

    pub(crate) async fn observe<C: tokio_postgres::GenericClient + Sync>(
        &self,
        client: &C,
    ) -> Result<Sample, Error> {
        timeout(Duration::from_secs(3), self.0.time.observe(client)).await?
    }

    pub fn connections(&self) -> &Connections {
        &self.0.connections
    }

    pub(crate) fn read_budget(&self) -> read::Budget {
        self.0.read
    }

    /// Request admission shares this closure with the clock and the generation fence.
    pub(crate) fn admission(&self) -> watch::Sender<bool> {
        self.0.admission.clone()
    }

    pub(crate) fn close(&self) {
        self.0.admission.send_replace(true);
    }

    /// The generation this process advanced inside its exclusive startup barrier.
    pub(crate) fn generation(&self) -> Option<i64> {
        *lock(&self.0.generation)
    }

    #[cfg(test)]
    pub(crate) async fn clock_session(&self) -> tokio::sync::MutexGuard<'_, Session> {
        self.0.clock.session().await
    }

    pub(crate) fn fence(&self, generation: i64) {
        *lock(&self.0.generation) = Some(generation);
    }

    /// The application must drain every consumer before closing shared resources.
    /// Interrupted background writes are ended before the instance session is released.
    pub async fn stop(&self) -> Result<(), Error> {
        let mut result = self.0.termination.lock().await;
        self.0.stopping.send_replace(true);
        let mut monitor = self.0.monitor.lock().await;
        if let Some(task) = monitor.as_mut() {
            let completion = task.await;
            monitor.take();
            *result = match completion {
                Ok(result) => result,
                Err(error) => {
                    self.0.clock.close();
                    Err(Error::Worker(error.into()))
                }
            };
        }
        // Also end the driver after an unexpected maintenance task failure. The task handle
        // stays owned across a cancelled stop wait, and completed results are never repolled.
        if let Err(error) = self.0.clock.stop(self.0.instance).await {
            *result = Err(match mem::replace(&mut *result, Ok(())) {
                Ok(()) => error.into(),
                Err(cause) => cause.combine(Err(error)),
            });
        }
        self.0.connections.close().await;
        mem::replace(&mut *result, Ok(()))
    }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        // The generation is a plain value; a panicking writer cannot leave it half-written.
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The instance lock lives exactly as long as the session, so either signal ends admission.
async fn monitor(
    clock: Arc<Clock>,
    mut ended: Option<Ended>,
    mut stopping: watch::Receiver<bool>,
) -> Result<(), Error> {
    let instance = ended.is_some();
    let mut heartbeat = interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut persistence = interval_at(Instant::now() + PERSIST, PERSIST);
    persistence.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let (stage, kind, error) = loop {
        tokio::select! {
            biased;
            _ = stopping.changed() => {
                let result = clock.persist(Instant::now() + clock::BUDGET).await;
                let ended = clock.stop(instance).await;
                return match result {
                    Ok(()) => Ok(ended?),
                    Err(error) => Err(error.combine(ended)),
                };
            }
            () = async {
                match ended.as_mut() {
                    Some(ended) => ended.wait().await,
                    None => std::future::pending().await,
                }
            } => {
                clock.close();
                break ("instance_lock", "instance_lost", Error::ClockClosed);
            }
            _ = heartbeat.tick(), if instance => {
                if let Err(error) = clock.heartbeat().await {
                    break ("instance_lock", "instance_lost", error);
                }
            }
            due = persistence.tick() => {
                if let Err(error) = clock.persist(due + clock::BUDGET).await {
                    break ("trusted_time", "time_persistence", error);
                }
            }
        }
    };
    tracing::error!(
        target: "domain::runtime::instance",
        stage,
        kind,
        error = ?error,
        "runtime maintenance failed; admission closed",
    );
    Err(error.combine(clock.stop(instance).await))
}

struct Inner {
    connections: Connections,
    clock: Arc<Clock>,
    time: Arc<Time>,
    instance: bool,
    read: read::Budget,
    termination: Mutex<Result<(), Error>>,
    admission: watch::Sender<bool>,
    generation: std::sync::Mutex<Option<i64>>,
    stopping: watch::Sender<bool>,
    monitor: Mutex<Option<JoinHandle<Result<(), Error>>>>,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{future::Future, task::Poll, time::Duration};

    use platform::database::{self, Connections, credentials::Password, settings};
    use rustls::RootCertStore;
    use tokio::{runtime::Builder, time::timeout};

    use crate::runtime::{Context, Error};

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
                    let context = Context::assemble(
                        connections,
                        session,
                        crate::database::read::Budget::default(),
                        None,
                    );
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
