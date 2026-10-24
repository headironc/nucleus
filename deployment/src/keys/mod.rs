//! Offline key registration and commit recovery.

pub mod credentials;

use std::fmt;

use domain::identity::keys::{
    database::{self, State},
    metadata::Metadata,
};
use platform::database::Connections;
use thiserror::Error;
use tokio::time::timeout;
use tokio_postgres::IsolationLevel;

use crate::OPERATION_BUDGET;

/// Initialize in one transaction and read back an uncertain result without writing again.
pub async fn initialize(connections: &Connections, metadata: &Metadata) -> Result<(), Error> {
    let mut session = connections.recover().await?;
    let mut phase = Phase::Ready;
    let result = timeout(OPERATION_BUDGET, async {
        let owner: bool = session
            .client()
            .query_one(
                r#"SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace
                 WHERE nspname = 'security' AND nspowner = current_user::regrole)"#,
                &[],
            )
            .await?
            .try_get(0)?;
        if !owner {
            return Err(Error::Identity);
        }
        let transaction = session
            .client_mut()
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .start()
            .await?;
        phase = Phase::Uncertain;
        let cause = match database::initialize(metadata, &transaction).await {
            Ok(()) => {
                transaction.commit().await?;
                return Ok::<_, Error>(());
            }
            Err(error) => error,
        };
        if let Err(source) = transaction.rollback().await {
            return Err(Error::Rollback { cause, source });
        }
        phase = Phase::RolledBack;
        Err(cause.into())
    })
    .await;
    let ended = session.terminate().await;
    // terminate has awaited the driver even if it reports a prior transport failure.
    let cause = match result {
        Ok(Ok(())) => {
            ended?;
            return Ok(());
        }
        Ok(Err(error)) => error,
        Err(error) => error.into(),
    };
    let cause = cause.combine(ended);
    if !matches!(phase, Phase::Uncertain) {
        return Err(cause);
    }
    match reconcile(connections, metadata).await {
        Ok(State::Matching) => Ok(()),
        Ok(State::Empty) => Err(Error::NotCommitted(Box::new(cause))),
        Err(recovery) => Err(Error::Unknown {
            cause: Box::new(cause),
            recovery: Box::new(recovery),
        }),
    }
}

async fn reconcile(connections: &Connections, metadata: &Metadata) -> Result<State, Error> {
    let mut session = connections.recover().await?;
    let result = timeout(OPERATION_BUDGET, async {
        let transaction = session
            .client_mut()
            .build_transaction()
            .isolation_level(IsolationLevel::ReadCommitted)
            .start()
            .await?;
        let state = database::inspect_locked(metadata, &transaction).await?;
        transaction.rollback().await?;
        Ok::<_, Error>(state)
    })
    .await;
    let ended = session.terminate().await;
    let error = match result {
        Ok(Ok(state)) => {
            ended?;
            return Ok(state);
        }
        Ok(Err(error)) => error,
        Err(error) => error.into(),
    };
    Err(error.combine(ended))
}

enum Phase {
    Ready,
    RolledBack,
    Uncertain,
}

#[derive(Error)]
pub enum Error {
    #[error("key initialization requires the schema owner")]
    Identity,
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("deployment database operation failed")]
    Database(#[from] tokio_postgres::Error),
    #[error("{0}")]
    Metadata(#[from] database::Error),
    #[error("cannot confirm rollback after key metadata failure")]
    Rollback {
        cause: database::Error,
        #[source]
        source: tokio_postgres::Error,
    },
    #[error("deployment operation budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("key initialization was not committed")]
    NotCommitted(#[source] Box<Error>),
    #[error("key initialization outcome cannot be confirmed")]
    Unknown {
        #[source]
        cause: Box<Error>,
        recovery: Box<Error>,
    },
    #[error("key initialization and connection termination failed")]
    Combined {
        #[source]
        cause: Box<Error>,
        termination: platform::database::Error,
    },
}

impl Error {
    fn combine(self, termination: Result<(), platform::database::Error>) -> Self {
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
            Self::Metadata(source) => formatter.debug_tuple("Metadata").field(source).finish(),
            Self::Rollback { cause, .. } => formatter
                .debug_struct("Rollback")
                .field("cause", cause)
                .field("rollback", &"database rollback failed")
                .finish(),
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
    use std::time::Duration;

    use domain::identity::keys::database::{self, State};
    use platform::database::{Session, User};
    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        keys::{initialize, reconcile},
        migration,
        tests::metadata,
    };

    #[test]
    fn diagnostics_preserve_both_safe_failure_categories() {
        let error = crate::keys::Error::Unknown {
            cause: Box::new(crate::keys::Error::Metadata(database::Error::Mismatch)),
            recovery: Box::new(crate::keys::Error::Identity),
        };
        let diagnostic = format!("{error:?}");
        assert!(
            diagnostic.contains("cause: Metadata(key metadata does not match loaded key sets)")
        );
        assert!(diagnostic.contains("recovery: key initialization requires the schema owner"));
        let original = std::error::Error::source(&error).expect("original cause");
        assert!(matches!(
            original
                .source()
                .expect("metadata cause")
                .downcast_ref::<database::Error>(),
            Some(database::Error::Mismatch)
        ));
    }

    #[test]
    fn initializes_atomically_and_clerk_can_only_verify() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            let name = crate::tests::database(&server).await;
            let connections = server.connections(&name, "steward");
            migration::run(
                &connections,
                &User::new("clerk".into()).expect("runtime user"),
            )
            .await
            .expect("migration");
            let expected = metadata(0, false);
            initialize(&connections, &expected)
                .await
                .expect("initialize");
            initialize(&connections, &expected)
                .await
                .expect("idempotent initialization");
            let clerk = server
                .connections(&name, "clerk")
                .recover()
                .await
                .expect("clerk");
            database::verify(&expected, clerk.client())
                .await
                .expect("verify metadata");
            assert!(
                database::verify(&metadata(1, false), clerk.client())
                    .await
                    .is_err()
            );
            assert!(
                database::verify(&metadata(0, true), clerk.client())
                    .await
                    .is_err()
            );
            for table in [
                "security.challenges",
                "security.mobiles",
                "security.encryptions",
                "security.messages",
            ] {
                for statement in [
                    format!("INSERT INTO {table} VALUES (8, decode(repeat('00',32),'hex'), false)"),
                    format!("UPDATE {table} SET fingerprint = decode(repeat('00',32),'hex')"),
                    format!("UPDATE {table} SET active = false"),
                    format!("DELETE FROM {table}"),
                    format!("TRUNCATE {table} CASCADE"),
                ] {
                    let error = clerk
                        .client()
                        .batch_execute(&statement)
                        .await
                        .expect_err("key metadata is read-only");
                    assert_eq!(
                        error.code(),
                        Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
                    );
                }
            }
            for sql in [
                r#"CREATE TABLE security.forbidden (id integer)"#,
                r#"CREATE TABLE identity.forbidden (id integer)"#,
                r#"SELECT * FROM deployment.migrations"#,
                "SET ROLE steward",
            ] {
                let error = clerk
                    .client()
                    .batch_execute(sql)
                    .await
                    .expect_err("clerk denied");
                assert_eq!(
                    error.code(),
                    Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
                );
            }
            clerk.terminate().await.expect("end clerk");
            assert!(initialize(&connections, &metadata(1, false)).await.is_err());
            let session = connections.recover().await.expect("steward");
            database::verify(&expected, session.client())
                .await
                .expect("unchanged after rejection");
            for table in [
                "security.challenges",
                "security.mobiles",
                "security.encryptions",
                "security.messages",
            ] {
                for (statement, state) in [
                    (
                        format!(
                            "INSERT INTO {table} VALUES (-1, decode(repeat('00',32),'hex'), false)"
                        ),
                        tokio_postgres::error::SqlState::CHECK_VIOLATION,
                    ),
                    (
                        format!("INSERT INTO {table} VALUES (9, decode('00','hex'), false)"),
                        tokio_postgres::error::SqlState::CHECK_VIOLATION,
                    ),
                    (
                        format!(
                            "INSERT INTO {table} VALUES (0, decode(repeat('00',32),'hex'), false)"
                        ),
                        tokio_postgres::error::SqlState::UNIQUE_VIOLATION,
                    ),
                    (
                        format!(
                            "INSERT INTO {table} VALUES (9, decode(repeat('00',32),'hex'), true)"
                        ),
                        tokio_postgres::error::SqlState::UNIQUE_VIOLATION,
                    ),
                    (
                        format!("UPDATE {table} SET active = true WHERE id = 1"),
                        tokio_postgres::error::SqlState::UNIQUE_VIOLATION,
                    ),
                    (
                        format!("UPDATE {table} SET active = NULL WHERE id = 1"),
                        tokio_postgres::error::SqlState::NOT_NULL_VIOLATION,
                    ),
                ] {
                    let error = session
                        .client()
                        .batch_execute(&statement)
                        .await
                        .expect_err("invalid key metadata rejected");
                    assert_eq!(error.code(), Some(&state));
                }
            }
            session.terminate().await.expect("end steward");
        });
    }

    #[test]
    fn rejects_incomplete_and_extra_versions_for_every_purpose_without_repairing_them() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            for table in ["security.challenges", "security.mobiles", "security.encryptions", "security.messages"] {
                for (statement, count, active) in [
                    (format!("DELETE FROM {table}"), 0_i64, 0_i64),
                    (format!("UPDATE {table} SET active = false"), 2, 0),
                    (format!("INSERT INTO {table} VALUES (9, decode(repeat('00',32),'hex'), false)"), 3, 1),
                ] {
                    let name = crate::tests::database(&server).await;
                    let connections = server.connections(&name, "steward");
                    migration::run(&connections, &User::new("clerk".into()).expect("runtime user")).await.expect("migration");
                    let expected = metadata(0, false);
                    initialize(&connections, &expected).await.expect("initialize");
                    let session = connections.recover().await.expect("owner");
                    session.client().batch_execute(&statement).await.expect("inconsistent fixture");
                    session.terminate().await.expect("end fixture session");
                    assert!(matches!(initialize(&connections, &expected).await,
                        Err(crate::keys::Error::Metadata(database::Error::Mismatch))));
                    let runtime = server.connections(&name, "clerk").recover().await.expect("runtime");
                    assert!(matches!(database::verify(&expected, runtime.client()).await, Err(database::Error::Mismatch)));
                    let row = runtime.client().query_one(&format!("SELECT count(*) AS total, count(*) FILTER (WHERE active) AS active FROM {table}"), &[]).await.expect("unchanged metadata");
                    assert_eq!(row.get::<_, i64>("total"), count);
                    assert_eq!(row.get::<_, i64>("active"), active);
                    runtime.terminate().await.expect("end runtime");
                }
            }
        });
    }

    #[test]
    fn recovers_committed_and_rolled_back_initialization_without_writes() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            let name = crate::tests::database(&server).await;
            let connections = server.connections(&name, "steward");
            migration::run(
                &connections,
                &User::new("clerk".into()).expect("runtime user"),
            )
            .await
            .expect("migration");
            let expected = metadata(0, false);
            let mut session = connections.recover().await.expect("session");
            let transaction = session
                .client_mut()
                .transaction()
                .await
                .expect("transaction");
            database::initialize(&expected, &transaction)
                .await
                .expect("uncommitted metadata");
            drop(transaction);
            session.terminate().await.expect("old driver ended");
            assert_eq!(
                reconcile(&connections, &expected)
                    .await
                    .expect("rollback readback"),
                State::Empty
            );

            let mut session = connections.recover().await.expect("session");
            let transaction = session
                .client_mut()
                .transaction()
                .await
                .expect("transaction");
            database::initialize(&expected, &transaction)
                .await
                .expect("metadata");
            // Simulate losing the successful result at the caller boundary, not a wire fault.
            transaction.commit().await.expect("server committed");
            session.terminate().await.expect("old driver ended");
            assert_eq!(
                reconcile(&connections, &expected)
                    .await
                    .expect("commit readback"),
                State::Matching
            );
            let session = connections.recover().await.expect("session");
            session
                .client()
                .batch_execute("UPDATE security.challenges SET active = false")
                .await
                .expect("incomplete fixture");
            session.terminate().await.expect("end driver");
            assert!(initialize(&connections, &expected).await.is_err());
        });
    }

    #[test]
    fn commit_rejection_is_read_back_as_uncommitted() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            let name = crate::tests::database(&server).await;
            let connections = server.connections(&name, "steward");
            migration::run(&connections, &User::new("clerk".into()).expect("runtime user")).await.expect("migration");
            let session = connections.recover().await.expect("session");
            session.client().batch_execute(
                r#"CREATE FUNCTION identity.reject_commit() RETURNS trigger LANGUAGE plpgsql AS
                 $$ BEGIN RAISE EXCEPTION 'fictional-sensitive-detail' USING ERRCODE = '23514'; END $$;
                 CREATE CONSTRAINT TRIGGER reject_commit AFTER INSERT ON security.encryptions
                 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.reject_commit()"#,
            ).await.expect("commit failure fixture");
            session.terminate().await.expect("end fixture driver");
            let expected = metadata(0, false);
            let error = initialize(&connections, &expected).await.expect_err("commit rejected");
            assert!(matches!(error, crate::keys::Error::NotCommitted(_)));
            assert!(!format!("{error:?}").contains("sensitive"));
            assert_eq!(reconcile(&connections, &expected).await.expect("readback"), State::Empty);
            assert!(matches!(initialize(&server.connections(&name, "clerk"), &expected).await,
                Err(crate::keys::Error::Identity)));
        });
    }

    #[test]
    fn initialization_waits_for_original_transaction_and_rechecks() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            let name = crate::tests::database(&server).await;
            let first = server.connections(&name, "steward");
            let second = server.connections(&name, "steward");
            migration::run(&first, &User::new("clerk".into()).expect("runtime user"))
                .await
                .expect("migration");
            let expected = metadata(0, false);
            let mut session = first.recover().await.expect("first session");
            let transaction = session
                .client_mut()
                .transaction()
                .await
                .expect("first transaction");
            database::initialize(&expected, &transaction)
                .await
                .expect("first initialization");
            let waiter =
                tokio::spawn(async move { initialize(&second, &metadata(0, false)).await });
            let inspector = server
                .connections(&name, "postgres")
                .recover()
                .await
                .expect("inspector");
            wait_for_lock(&inspector, &name).await;
            assert!(!waiter.is_finished());
            transaction.commit().await.expect("commit first");
            timeout(Duration::from_secs(5), waiter)
                .await
                .expect("waiter bound")
                .expect("task")
                .expect("matching retry");
            database::verify(&expected, session.client())
                .await
                .expect("one complete registry");
            session.terminate().await.expect("end first");
            inspector.terminate().await.expect("end inspector");
        });
    }

    async fn wait_for_lock(inspector: &Session, name: &str) {
        timeout(Duration::from_secs(5), async {
            loop {
                let waiting: bool = inspector.client().query_one(
                    r#"SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname = $1 AND wait_event = 'advisory')"#,
                    &[&name],
                ).await.expect("observe lock wait").get(0);
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("waiter reached database lock");
    }
}
