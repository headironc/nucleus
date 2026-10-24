//! Embedded, versioned SQL migrations under the deployment identity.

use std::fmt;

use platform::database::{self, Connections, User};
use refinery::{Migration, Runner};
use thiserror::Error;
use tokio::time::timeout;
use tokio_postgres::Client;

use crate::OPERATION_BUDGET;

fn runner() -> Result<Runner, Error> {
    let mut runner = Runner::new(&[
        Migration::unapplied("V1__keys", include_str!("../migrations/V1__keys.sql"))?,
        Migration::unapplied(
            "V2__challenge_acceptance",
            include_str!("../migrations/V2__challenge_acceptance.sql"),
        )?,
        Migration::unapplied(
            "V3__message_sending",
            include_str!("../migrations/V3__message_sending.sql"),
        )?,
        Migration::unapplied(
            "V4__retention",
            include_str!("../migrations/V4__retention.sql"),
        )?,
    ])
    .set_grouped(true)
    .set_abort_divergent(true)
    .set_abort_missing(true);
    runner.set_migration_table_name("deployment.migrations");
    Ok(runner)
}

/// This lock spans Refinery's separate history and migration transactions.
async fn lock(client: &Client, runtime_user: &User) -> Result<(), Error> {
    client
        .query_one(
            r#"SELECT set_config('nucleus.deployment_user', current_user, false),
         set_config('nucleus.runtime_user', $1, false)"#,
            &[runtime_user],
        )
        .await?;
    client.batch_execute(include_str!("../roles.sql")).await?;
    let permitted: bool = client
        .query_one(
            r#"SELECT has_database_privilege(current_user, current_database(), 'CREATE')
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles role
                 WHERE pg_has_role($1::text, role.oid, 'MEMBER') AND (
                     has_database_privilege(role.oid, current_database(), 'CREATE,TEMP')
                     OR EXISTS (SELECT 1 FROM pg_catalog.pg_namespace
                         WHERE has_schema_privilege(role.oid, oid, 'CREATE'))))
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace
                 WHERE nspname = 'deployment' AND nspowner <> current_user::regrole)"#,
            &[runtime_user],
        )
        .await?
        .try_get(0)?;
    if !permitted {
        return Err(Error::Identity);
    }
    client
        .batch_execute(r#"SELECT pg_catalog.pg_advisory_lock(1314210636, 2)"#)
        .await?;
    Ok(())
}

/// Apply only embedded migrations. A failed attempt is followed by readback, never reapplication.
pub async fn run(connections: &Connections, runtime_user: &User) -> Result<(), Error> {
    let mut session = connections.recover().await?;
    let mut started = false;
    let result = timeout(OPERATION_BUDGET, async {
        lock(session.client(), runtime_user).await?;
        started = true;
        session
            .client()
            .batch_execute(
                r#"CREATE SCHEMA IF NOT EXISTS deployment;
             REVOKE ALL ON SCHEMA deployment FROM PUBLIC"#,
            )
            .await?;
        runner()?.run_async(session.client_mut()).await?;
        Ok::<_, Error>(())
    })
    .await;
    let ended = session.terminate().await;
    let cause = match result {
        Ok(Ok(())) => {
            ended?;
            return Ok(());
        }
        Ok(Err(error)) => error,
        Err(error) => error.into(),
    };
    let cause = cause.combine(ended);
    if !started {
        return Err(cause);
    }
    match reconcile(connections, runtime_user).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::NotCommitted(Box::new(cause))),
        Err(recovery) => Err(Error::Unknown {
            cause: Box::new(cause),
            recovery: Box::new(recovery),
        }),
    }
}

async fn reconcile(connections: &Connections, runtime_user: &User) -> Result<bool, Error> {
    let mut session = connections.recover().await?;
    let result = timeout(OPERATION_BUDGET, async {
        lock(session.client(), runtime_user).await?;
        let exists: bool = session
            .client()
            .query_one(
                r#"SELECT pg_catalog.to_regclass('deployment.migrations') IS NOT NULL"#,
                &[],
            )
            .await?
            .try_get(0)?;
        if !exists {
            return Ok(false);
        }
        let runner = runner()?;
        let applied = runner
            .get_applied_migrations_async(session.client_mut())
            .await?;
        let expected = runner.get_migrations();
        if applied.is_empty() {
            return Ok(false);
        }
        if applied.len() != expected.len()
            || applied.iter().zip(expected).any(|(a, b)| {
                a.version() != b.version() || a.name() != b.name() || a.checksum() != b.checksum()
            })
        {
            return Err(Error::History);
        }
        Ok::<_, Error>(true)
    })
    .await;
    let ended = session.terminate().await;
    let error = match result {
        Ok(Ok(applied)) => {
            ended?;
            return Ok(applied);
        }
        Ok(Err(error)) => error,
        Err(error) => error.into(),
    };
    Err(error.combine(ended))
}

#[derive(Error)]
pub enum Error {
    #[error("database deployment permissions are invalid")]
    Identity,
    #[error("migration history differs from embedded migrations")]
    History,
    #[error("{0}")]
    Connection(#[from] database::Error),
    #[error("migration database operation failed")]
    Database(#[from] tokio_postgres::Error),
    #[error("migration execution failed")]
    Migration(#[from] refinery::Error),
    #[error("migration budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("migration was not committed")]
    NotCommitted(#[source] Box<Error>),
    #[error("migration outcome cannot be confirmed")]
    Unknown {
        #[source]
        cause: Box<Error>,
        recovery: Box<Error>,
    },
    #[error("migration operation and connection termination failed")]
    Combined {
        #[source]
        cause: Box<Error>,
        termination: database::Error,
    },
}

impl Error {
    fn combine(self, termination: Result<(), database::Error>) -> Self {
        match termination {
            Ok(()) => self,
            Err(termination) => Self::Combined {
                cause: Box::new(self),
                termination,
            },
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(source) => formatter.debug_tuple("Connection").field(source).finish(),
            Self::NotCommitted(cause) => {
                formatter.debug_tuple("NotCommitted").field(cause).finish()
            }
            Self::Unknown { cause, recovery } => formatter
                .debug_struct("Unknown")
                .field("cause", cause)
                .field("recovery", recovery)
                .finish(),
            Self::Combined { cause, termination } => formatter
                .debug_struct("Combined")
                .field("cause", cause)
                .field("termination", termination)
                .finish(),
            _ => fmt::Display::fmt(self, formatter),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use platform::database::User;
    use tokio::runtime::Runtime;

    use crate::migration::{reconcile, run, runner};

    #[test]
    fn diagnostics_preserve_original_and_recovery_failures() {
        let error = crate::migration::Error::Unknown {
            cause: Box::new(crate::migration::Error::NotCommitted(Box::new(
                crate::migration::Error::History,
            ))),
            recovery: Box::new(crate::migration::Error::Identity),
        };
        let diagnostic = format!("{error:?}");
        assert!(
            diagnostic.contains(
                "cause: NotCommitted(migration history differs from embedded migrations)"
            )
        );
        assert!(diagnostic.contains("recovery: database deployment permissions are invalid"));
    }

    #[test]
    fn migration_is_repeatable_and_rejects_changed_history() {
        let server = crate::tests::server();
        let user = User::new("clerk".into()).expect("runtime user");
        Runtime::new().expect("runtime").block_on(async {
            let name = crate::tests::database(&server).await;
            let connections = server.connections(&name, "steward");
            assert!(
                !reconcile(&connections, &user)
                    .await
                    .expect("empty database")
            );
            run(&connections, &user).await.expect("first migration");
            run(&connections, &user).await.expect("repeat migration");
            let other = server.connections(&name, "steward");
            let (first, second) = tokio::join!(run(&connections, &user), run(&other, &user));
            first.expect("first concurrent runner");
            second.expect("second concurrent runner");
            assert!(
                reconcile(&connections, &user)
                    .await
                    .expect("read committed history")
            );
            let mut session = connections.recover().await.expect("session");
            let applied = runner()
                .expect("embedded migration")
                .get_applied_migrations_async(session.client_mut())
                .await
                .expect("history");
            assert_eq!(applied.len(), 4);
            assert_eq!(applied[0].version(), 1);
            assert_eq!(applied[1].version(), 2);
            assert_eq!(applied[2].version(), 3);
            assert_eq!(applied[3].version(), 4);
            session
                .client()
                .batch_execute(r#"UPDATE deployment.migrations SET checksum = '0'"#)
                .await
                .expect("divergent history fixture");
            session.terminate().await.expect("end fixture driver");
            assert!(run(&connections, &user).await.is_err());
            assert!(
                run(&server.connections(&name, "clerk"), &user)
                    .await
                    .is_err()
            );
        });
    }

    #[test]
    fn failed_migration_rolls_back_schema_and_history_together() {
        let server = crate::tests::server();
        let user = User::new("clerk".into()).expect("runtime user");
        Runtime::new().expect("runtime").block_on(async {
            let name = crate::tests::database(&server).await;
            let connections = server.connections(&name, "steward");
            let session = connections.recover().await.expect("session");
            // Collision makes the real migration fail rather than substituting test SQL.
            session
                .client()
                .batch_execute(r#"CREATE SCHEMA identity"#)
                .await
                .expect("collision fixture");
            session.terminate().await.expect("end fixture driver");
            assert!(run(&connections, &user).await.is_err());
            assert!(
                !reconcile(&connections, &user)
                    .await
                    .expect("no migration committed")
            );
            let session = connections.recover().await.expect("session");
            let absent: bool = session
                .client()
                .query_one(
                    r#"SELECT pg_catalog.to_regclass('security.challenges') IS NULL"#,
                    &[],
                )
                .await
                .expect("catalog")
                .get(0);
            assert!(absent);
            session.terminate().await.expect("end fixture driver");
        });
    }
}
