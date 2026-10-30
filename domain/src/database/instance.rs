//! The database session lock that admits one application or deployment command per database.

use std::time::Duration;

use platform::database::{self, Connections, Ended, Session};
use tokio::time::timeout;
use tokio_postgres::GenericClient;

use crate::{
    database::verification::{Stage, Waiting},
    runtime::Error,
};

/// Distinct from the runtime barrier `(1314210636, 1)` and the migration lock `(1314210636, 2)`.
const LOCK: &str = "SELECT pg_catalog.pg_try_advisory_lock(1314210636, 3)";

/// A dedicated session holding the instance lock; the lock ends with the session.
pub struct Instance {
    session: Session,
    ended: Ended,
}

impl Instance {
    /// Refuse immediately when another process holds the lock; never wait for it.
    pub async fn hold(connections: &Connections) -> Result<Self, Error> {
        let (session, ended) = Stage::ClockConnection.runtime(
            Waiting::Connection,
            match timeout(Duration::from_secs(3), connections.instance()).await {
                Ok(result) => result.map_err(Error::from),
                Err(error) => Err(error.into()),
            },
        )?;
        let held = Stage::InstanceLock.runtime(
            Waiting::Query,
            match timeout(
                Duration::from_secs(3),
                session.client().query_one(LOCK, &[]),
            )
            .await
            {
                Ok(Ok(row)) => row.try_get::<_, bool>(0).map_err(Error::from),
                Ok(Err(error)) => Err(error.into()),
                Err(error) => Err(error.into()),
            },
        );
        let error = match held {
            Ok(true) => return Ok(Self { session, ended }),
            Ok(false) => Error::InstanceHeld,
            Err(error) => error,
        };
        Err(error.combine(session.terminate().await))
    }

    /// Advance the generation fence inside the exclusive barrier, before any change.
    ///
    /// The barrier outwaits every transaction of an earlier generation, and the earlier
    /// process can no longer begin one afterwards. A database whose migrations have not
    /// created the fence has no application that could hold a generation. On failure the
    /// caller releases the instance, which ends the open transaction with the session.
    pub async fn advance(&self) -> Result<Fence, Error> {
        let client = self.session.client();
        let advanced = async {
            client
                .batch_execute(
                    "BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL statement_timeout = '60s'; \
                     SELECT pg_catalog.pg_advisory_xact_lock(1314210636, 1)",
                )
                .await?;
            let fenced: bool = client
                .query_one(
                    "SELECT pg_catalog.to_regclass('security.instances') IS NOT NULL",
                    &[],
                )
                .await?
                .try_get(0)?;
            let generation = if fenced {
                Some(advance(client).await?)
            } else {
                None
            };
            client.batch_execute("COMMIT").await?;
            Ok::<_, Error>(Fence(generation))
        };
        match timeout(Duration::from_secs(90), advanced).await {
            Ok(result) => result,
            Err(error) => Err(error.into()),
        }
    }

    /// Unlock, then end the session and await its driver.
    ///
    /// The server releases a closed session's locks only when its backend exits, after
    /// the client has gone; unlocking first makes a normal release visible at once.
    pub async fn release(self) -> Result<(), database::Error> {
        unlock(&self.session).await;
        self.session.terminate().await
    }

    pub(crate) fn into_parts(self) -> (Session, Ended) {
        (self.session, self.ended)
    }
}

/// Best effort: a failed or dead session releases the lock when its backend exits.
pub(crate) async fn unlock(session: &Session) {
    let _ = timeout(
        Duration::from_secs(1),
        session
            .client()
            .query_one("SELECT pg_catalog.pg_advisory_unlock(1314210636, 3)", &[]),
    )
    .await;
}

/// The generation a deployment command advanced; only [`Instance::advance`] creates one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fence(Option<i64>);

impl Fence {
    pub fn generation(self) -> Option<i64> {
        self.0
    }

    /// Admit a deployment write transaction only while no later process has started.
    ///
    /// Call this first in the open READ COMMITTED transaction of `client`. It takes the
    /// exclusive runtime barrier before the generation row, in the order every other
    /// holder uses, so a later start waits for this transaction and the transaction
    /// sees any start that committed before it. On failure the caller rolls back; the
    /// transaction has written nothing.
    pub async fn require(self, client: &impl GenericClient) -> Result<(), Error> {
        client
            .batch_execute("SELECT pg_catalog.pg_advisory_xact_lock(1314210636, 1)")
            .await?;
        let generation: i64 = client
            .query_one(
                "SELECT generation FROM security.instances WHERE id FOR SHARE",
                &[],
            )
            .await?
            .try_get(0)?;
        if self.0 != Some(generation) {
            return Err(Error::InstanceSuperseded);
        }
        Ok(())
    }
}

/// The caller holds the exclusive barrier in the open transaction of `client`.
pub(crate) async fn advance(client: &tokio_postgres::Client) -> Result<i64, tokio_postgres::Error> {
    client
        .query_one(
            "UPDATE security.instances SET generation = generation + 1 WHERE id RETURNING generation",
            &[],
        )
        .await?
        .try_get(0)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        database::{Barrier, instance::Instance, transaction},
        identity::{
            authentication::{Database, server},
            operation,
        },
        runtime::{Context, Error},
    };

    async fn backend(context: &Context) -> i32 {
        let session = context.clock_session().await;
        session
            .client()
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .expect("instance backend")
            .get(0)
    }

    async fn terminate(database: &Database, pid: i32) {
        let admin = database
            .connections("postgres")
            .recover()
            .await
            .expect("fixture admin");
        let ended: bool = admin
            .client()
            .query_one("SELECT pg_terminate_backend($1)", &[&pid])
            .await
            .expect("terminate backend")
            .get(0);
        assert!(ended);
        admin.terminate().await.expect("admin ended");
    }

    async fn closed(context: &Context) {
        let mut admission = context.admission().subscribe();
        timeout(Duration::from_secs(8), admission.wait_for(|closed| *closed))
            .await
            .expect("admission closes")
            .expect("admission channel");
    }

    /// A started process: the instance lock, the fenced startup barrier and its confirmation.
    async fn start(database: &Database) -> Context {
        let context = Context::hold(database.connections(&database.user))
            .await
            .expect("instance context");
        let barrier = Barrier::begin(context.clone()).await.expect("barrier");
        barrier.confirm().await.expect("confirm barrier");
        barrier.terminate().await.expect("barrier ended");
        context
    }

    async fn begin(
        context: &Context,
    ) -> (platform::database::Session, Result<(), transaction::Error>) {
        let session = context
            .connections()
            .recover()
            .await
            .expect("transaction session");
        let operation: operation::Id = uuid::Uuid::new_v4().to_string().parse().expect("operation");
        let result = transaction::begin(context, session.client(), b"test/fence", operation).await;
        (session, result)
    }

    #[test]
    fn a_second_holder_on_other_connections_is_refused_until_the_session_ends() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                let database = Database::new(server).await;
                let first = database.connections(&database.user);
                let holder = Instance::hold(&first).await.expect("first holder");
                for user in [database.user.as_str(), "postgres"] {
                    let other = database.connections(user);
                    assert!(matches!(
                        Instance::hold(&other).await,
                        Err(Error::InstanceHeld)
                    ));
                    assert!(matches!(
                        Context::hold(other).await,
                        Err(Error::InstanceHeld)
                    ));
                }
                holder.release().await.expect("release");
                // A normal release unlocks before ending the session, so it is free at once.
                let second = Instance::hold(&database.connections(&database.user))
                    .await
                    .expect("released lock is free");
                let pid: i32 = second
                    .session
                    .client()
                    .query_one("SELECT pg_backend_pid()", &[])
                    .await
                    .expect("backend")
                    .get(0);
                terminate(&database, pid).await;
                timeout(Duration::from_secs(5), async {
                    loop {
                        match Instance::hold(&database.connections(&database.user)).await {
                            Ok(third) => break third.release().await.expect("release third"),
                            Err(Error::InstanceHeld) => tokio::task::yield_now().await,
                            Err(error) => panic!("unexpected probe failure: {error:?}"),
                        }
                    }
                })
                .await
                .expect("an ended backend releases the lock");
                first.close().await;
            })
            .await
            .expect("instance lock budget");
        });
    }

    #[test]
    fn a_lost_instance_session_or_lock_closes_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::new(server).await;
                let context = Context::hold(database.connections(&database.user))
                    .await
                    .expect("instance context");
                assert!(!*context.admission().borrow());
                terminate(&database, backend(&context).await).await;
                closed(&context).await;
                assert!(
                    context.sample().await.is_err(),
                    "trusted time ends with the session"
                );
                context
                    .stop()
                    .await
                    .expect_err("the maintenance owner retains the backend failure");

                // A held session whose lock disappeared fails its next heartbeat.
                let context = Context::hold(database.connections(&database.user))
                    .await
                    .expect("restarted instance context");
                context
                    .clock_session()
                    .await
                    .client()
                    .query_one("SELECT pg_advisory_unlock(1314210636, 3)", &[])
                    .await
                    .expect("drop the lock from the session");
                closed(&context).await;
                assert!(context.sample().await.is_err());
                context
                    .stop()
                    .await
                    .expect_err("the maintenance owner retains the heartbeat failure");

                // A normal stop unlocks before ending the instance session.
                let context = Context::hold(database.connections(&database.user))
                    .await
                    .expect("third instance context");
                context.stop().await.expect("normal stop");
                Instance::hold(&database.connections(&database.user))
                    .await
                    .expect("a stopped context released the lock at once")
                    .release()
                    .await
                    .expect("release");
            })
            .await
            .expect("instance loss budget");
        });
    }

    #[test]
    fn a_later_start_outwaits_and_fences_an_earlier_generation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::new(server).await;
                let old = start(&database).await;
                // Accepted work holds the generation row shared inside its transaction.
                let (open, result) = begin(&old).await;
                result.expect("current generation begins");
                terminate(&database, backend(&old).await).await;
                closed(&old).await;
                let new = Context::hold(database.connections(&database.user))
                    .await
                    .expect("the lost session freed the lock");
                let mut startup = tokio::spawn(Barrier::begin(new.clone()));
                let pid: i32 = open
                    .client()
                    .query_one("SELECT pg_backend_pid()", &[])
                    .await
                    .expect("open transaction backend")
                    .get(0);
                let observer = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("observer");
                timeout(Duration::from_secs(5), async {
                    loop {
                        let blocked: bool = observer
                            .client()
                            .query_one(
                                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname = current_database() AND $1 = ANY(pg_catalog.pg_blocking_pids(pid)))",
                                &[&pid],
                            )
                            .await
                            .expect("blocked startup")
                            .get(0);
                        if blocked {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("startup waits for the earlier generation's transaction");
                assert!(timeout(Duration::from_millis(50), &mut startup).await.is_err());
                open.client().batch_execute("COMMIT").await.expect("earlier work ends");
                let barrier = timeout(Duration::from_secs(5), startup)
                    .await
                    .expect("startup proceeds")
                    .expect("startup task")
                    .expect("barrier");
                barrier.confirm().await.expect("confirm");
                barrier.terminate().await.expect("barrier ended");
                open.terminate().await.expect("open session ended");

                let (session, result) = begin(&old).await;
                assert!(matches!(result, Err(transaction::Error::Superseded)));
                assert!(!session.client().is_closed());
                let in_transaction: String = session
                    .client()
                    .query_one("SELECT pg_catalog.current_setting('transaction_isolation')", &[])
                    .await
                    .expect("session usable")
                    .get(0);
                assert_eq!(in_transaction, "read committed");
                session.terminate().await.expect("old session ended");
                let (session, result) = begin(&new).await;
                result.expect("the new generation begins");
                session.terminate().await.expect("new session ended");
                observer.terminate().await.expect("observer ended");
                assert!(old.stop().await.is_err());
                new.stop().await.expect("new stopped");
            })
            .await
            .expect("generation fence budget");
        });
    }

    #[test]
    fn a_deployment_command_takes_over_and_fences_the_application() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::new(server).await;
                let application = start(&database).await;
                let before = application.generation().expect("fenced start");
                let deployment = database.connections("postgres");
                assert!(
                    matches!(Instance::hold(&deployment).await, Err(Error::InstanceHeld)),
                    "the running application excludes the deployment command"
                );
                terminate(&database, backend(&application).await).await;
                closed(&application).await;
                let command = Instance::hold(&deployment)
                    .await
                    .expect("deployment takes over");
                assert_eq!(
                    command.advance().await.expect("advance").generation(),
                    Some(before + 1)
                );
                assert!(
                    matches!(
                        Context::hold(database.connections(&database.user)).await,
                        Err(Error::InstanceHeld)
                    ),
                    "the deployment command excludes a new application"
                );
                let (session, result) = begin(&application).await;
                assert!(matches!(result, Err(transaction::Error::Superseded)));
                session.terminate().await.expect("old session ended");
                command.release().await.expect("command ended");
                deployment.close().await;
                assert!(application.stop().await.is_err());
            })
            .await
            .expect("deployment takeover budget");
        });
    }
}
