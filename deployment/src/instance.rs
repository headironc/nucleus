//! Every deployment command holds the database instance lock for its whole run.

use std::future::Future;

use domain::database::instance::{Fence, Instance};
use platform::database::{self, Connections};

/// The instance lock of a running deployment command and the generation it advanced.
pub struct Held {
    instance: Instance,
    fence: Fence,
}

impl Held {
    /// Every write transaction of the command requires this generation first.
    pub fn fence(&self) -> Fence {
        self.fence
    }

    pub async fn release(self) -> Result<(), database::Error> {
        self.instance.release().await
    }
}

/// Hold the instance lock and fence out every earlier generation before any change.
///
/// An application that lost its session lock may still have accepted work between
/// transactions; after the fence it can no longer begin one. If this command's own
/// session is lost and a later process starts, the command's next write transaction
/// fails its fence instead of writing.
pub async fn take(connections: &Connections) -> Result<Held, domain::runtime::Error> {
    let instance = Instance::hold(connections).await?;
    match instance.advance().await {
        Ok(fence) => Ok(Held { instance, fence }),
        Err(error) => Err(match instance.release().await {
            Ok(()) => error,
            Err(termination) => domain::runtime::Error::Combined {
                cause: Box::new(error),
                termination,
            },
        }),
    }
}

/// Run a command while no application or other deployment command can hold the lock.
///
/// The lock session ends after the command; a crash ends it with the process. Startup's
/// exclusive barrier still waits for any transaction the command left behind.
pub async fn exclusive<T, E, F>(
    connections: &Connections,
    command: impl FnOnce(Fence) -> F,
) -> Result<T, Error>
where
    E: std::error::Error + Send + Sync + 'static,
    F: Future<Output = Result<T, E>>,
{
    let instance = take(connections).await?;
    let result = command(instance.fence()).await;
    match (result, instance.release().await) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(termination)) => Err(Error::Completed(termination)),
        (Err(cause), Ok(())) => Err(Error::Command(Box::new(cause))),
        (Err(cause), Err(termination)) => Err(Error::Combined {
            cause: Box::new(cause),
            termination,
        }),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Held(#[from] domain::runtime::Error),
    #[error("{0}")]
    Command(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("the command completed but ending the instance session failed")]
    Completed(#[source] database::Error),
    #[error("the command and ending the instance session both failed")]
    Combined {
        #[source]
        cause: Box<dyn std::error::Error + Send + Sync>,
        termination: database::Error,
    },
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) mod tests {
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };

    use domain::runtime::Context;
    use platform::database::User;
    use tokio::{runtime::Runtime, time::timeout};

    use crate::instance::{Error, exclusive, take};

    async fn generation(server: &crate::tests::Server, name: &str) -> i64 {
        let observer = server
            .connections(name, "clerk")
            .recover()
            .await
            .expect("runtime observer");
        let generation = observer
            .client()
            .query_one("SELECT generation FROM security.instances", &[])
            .await
            .expect("current generation")
            .get(0);
        observer.terminate().await.expect("observer ended");
        generation
    }

    #[test]
    fn commands_and_the_application_exclude_each_other_and_commands_advance_the_fence() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let name = crate::tests::database(&server).await;
                let tool = server.connections(&name, "steward");
                // Before migrations no application can hold a generation; the lock still applies.
                let instance = take(&tool).await.expect("unmigrated database");
                assert!(matches!(
                    Context::hold(server.connections(&name, "clerk")).await,
                    Err(domain::runtime::Error::InstanceHeld)
                ));
                instance.release().await.expect("released");
                assert!(crate::tests::released(&server, &name).await);
                let user = User::new("clerk".into()).expect("runtime user");
                exclusive(&tool, |_| crate::migration::run(&tool, &user))
                    .await
                    .expect("migrations under the lock");
                assert!(crate::tests::released(&server, &name).await);
                assert_eq!(generation(&server, &name).await, 0);
                exclusive(&tool, |_| async { Ok::<_, std::io::Error>(()) })
                    .await
                    .expect("a later command");
                assert_eq!(
                    generation(&server, &name).await,
                    1,
                    "each command advances the fence"
                );
                assert!(crate::tests::released(&server, &name).await);

                let application = Context::hold(server.connections(&name, "clerk"))
                    .await
                    .expect("application holds the lock");
                let ran = AtomicBool::new(false);
                let refused = exclusive(&tool, |_| async {
                    ran.store(true, Ordering::SeqCst);
                    Ok::<_, std::io::Error>(())
                })
                .await
                .expect_err("the running application excludes the command");
                assert!(matches!(
                    refused,
                    Error::Held(domain::runtime::Error::InstanceHeld)
                ));
                assert!(
                    !ran.load(Ordering::SeqCst),
                    "a refused command changes nothing"
                );
                assert_eq!(generation(&server, &name).await, 1);
                application.stop().await.expect("application stopped");
                assert!(crate::tests::released(&server, &name).await);
                exclusive(&tool, |_| async { Ok::<_, std::io::Error>(()) })
                    .await
                    .expect("the stopped application released the lock");
                assert_eq!(generation(&server, &name).await, 2);
                tool.close().await;
            })
            .await
            .expect("mutual exclusion budget");
        });
    }

    /// End the instance backend of a running command, as a lost network or an operator would.
    pub(crate) async fn terminate_instance(server: &crate::tests::Server, name: &str) {
        let admin = server
            .connections(name, "postgres")
            .recover()
            .await
            .expect("fixture admin");
        let terminated: bool = admin
            .client()
            .query_one(
                "SELECT pg_terminate_backend(pid) FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND classid = 1314210636 AND objid = 3 AND objsubid = 2 AND granted AND database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())",
                &[],
            )
            .await
            .expect("terminate the instance backend")
            .get(0);
        assert!(terminated);
        timeout(Duration::from_secs(5), async {
            while admin
                .client()
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND classid = 1314210636 AND objid = 3 AND objsubid = 2 AND granted)",
                    &[],
                )
                .await
                .expect("instance lock observation")
                .get::<_, bool>(0)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the ended backend released the lock");
        admin.terminate().await.expect("admin ended");
    }

    #[test]
    fn key_initialization_that_lost_its_lock_cannot_write_after_a_later_start() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let name = crate::tests::database(&server).await;
                let tool = server.connections(&name, "steward");
                let user = User::new("clerk".into()).expect("runtime user");
                exclusive(&tool, |_| crate::migration::run(&tool, &user))
                    .await
                    .expect("migrations");
                assert!(crate::tests::released(&server, &name).await);
                let held = take(&tool).await.expect("key command lock");
                terminate_instance(&server, &name).await;
                // A later process takes the free lock and advances the generation.
                let later = take(&server.connections(&name, "steward"))
                    .await
                    .expect("later process");
                let error =
                    crate::keys::initialize(&tool, &crate::tests::metadata(0, false), held.fence())
                        .await
                        .expect_err("a superseded command cannot write");
                assert!(
                    matches!(
                        error,
                        crate::keys::Error::Fence(domain::runtime::Error::InstanceSuperseded)
                    ),
                    "{error:?}"
                );
                let observer = server
                    .connections(&name, "steward")
                    .recover()
                    .await
                    .expect("observer");
                let written: i64 = observer
                    .client()
                    .query_one("SELECT count(*) FROM security.keys", &[])
                    .await
                    .expect("key registration")
                    .get(0);
                assert_eq!(written, 0, "nothing was registered");
                observer.terminate().await.expect("observer ended");
                assert!(
                    held.release().await.is_err(),
                    "the ended backend reports its failure"
                );
                later.release().await.expect("later process ended");
                assert!(crate::tests::released(&server, &name).await);
                tool.close().await;
            })
            .await
            .expect("superseded key command budget");
        });
    }
}
