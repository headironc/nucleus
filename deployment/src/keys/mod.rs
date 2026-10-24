//! Offline key registration and commit recovery.

pub mod credentials;

use std::fmt;

use domain::{
    database::{
        instance::Fence,
        transaction::{Completion, EndError, Known, commit, rollback},
    },
    keys::{
        database::{self, State},
        metadata::Metadata,
    },
};
use platform::database::Connections;
use thiserror::Error;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::IsolationLevel;

use crate::OPERATION_BUDGET;

/// Initialize in one transaction and read back an uncertain result without writing again.
/// The fence comes from the instance lock this command holds; see [`crate::instance`].
pub async fn initialize(
    connections: &Connections,
    metadata: &Metadata,
    fence: Fence,
) -> Result<(), Error> {
    let mut session = connections.recover().await?;
    let mut phase = Phase::Ready;
    let deadline = Instant::now() + OPERATION_BUDGET;
    let result = async {
        let owner: bool = timeout_at(
            deadline,
            session.client().query_one(
                r#"SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace
                 WHERE nspname = 'security' AND nspowner = current_user::regrole)"#,
                &[],
            ),
        )
        .await??
        .try_get(0)?;
        if !owner {
            return Err(Error::Identity);
        }
        let transaction = timeout_at(
            deadline,
            session
                .client_mut()
                .build_transaction()
                .isolation_level(IsolationLevel::ReadCommitted)
                .start(),
        )
        .await??;
        // A refused fence ends this transaction unwritten with the session below.
        timeout_at(deadline, fence.require(&transaction))
            .await?
            .map_err(Error::Fence)?;
        phase = Phase::Uncertain;
        let cause = match timeout_at(deadline, database::initialize(metadata, &transaction)).await?
        {
            Ok(outcome) => {
                return Ok::<_, Error>(commit(outcome, deadline, transaction.commit()).await);
            }
            Err(error) => error,
        };
        let Known { value: cause, end } = rollback(cause, deadline, transaction.rollback()).await;
        match end {
            Ok(()) => {
                phase = Phase::RolledBack;
                Err(cause.into())
            }
            Err(source) => Err(Error::Rollback { cause, source }),
        }
    }
    .await;
    let ended = session.terminate().await;
    // terminate has awaited the driver even if it reports a prior transport failure.
    let cause = match result {
        Ok(Completion::Known(Known { value: (), end })) => {
            return match (end, ended) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(error), ended) => Err(Error::Completed {
                    cause: Box::new(Error::End(error).combine(ended)),
                }),
                (Ok(()), Err(error)) => Err(Error::Completed {
                    cause: Box::new(error.into()),
                }),
            };
        }
        Ok(Completion::Pending { error, .. }) => Error::End(error),
        Err(error) => error,
    };
    if matches!(phase, Phase::RolledBack) && ended.is_err() {
        return Err(Error::NotCommitted(Box::new(cause.combine(ended))));
    }
    let cause = cause.combine(ended);
    if !matches!(phase, Phase::Uncertain) {
        return Err(cause);
    }
    match reconcile(connections, metadata).await {
        Ok((
            Known {
                value: State::Matching,
                end: Ok(()),
            },
            Ok(()),
        )) => Ok(()),
        Ok((
            Known {
                value: State::Empty,
                end: Ok(()),
            },
            Ok(()),
        )) => Err(Error::NotCommitted(Box::new(cause))),
        Ok((
            Known {
                value: state,
                end: rollback,
            },
            termination,
        )) => Err(Error::Recovered {
            state,
            cause: Box::new(cause),
            rollback,
            termination,
        }),
        Err(recovery) => Err(Error::Unknown {
            cause: Box::new(cause),
            recovery: Box::new(recovery),
        }),
    }
}

async fn reconcile(
    connections: &Connections,
    metadata: &Metadata,
) -> Result<(Known<State>, Result<(), platform::database::Error>), Error> {
    let mut session = connections.recover().await?;
    let deadline = Instant::now() + OPERATION_BUDGET;
    let result = async {
        let transaction = timeout_at(
            deadline,
            session
                .client_mut()
                .build_transaction()
                .isolation_level(IsolationLevel::ReadCommitted)
                .start(),
        )
        .await??;
        let state =
            timeout_at(deadline, database::inspect_locked(metadata, &transaction)).await??;
        // The locked read determines the outcome before this read-only transaction ends.
        Ok::<_, Error>(rollback(state, deadline, transaction.rollback()).await)
    }
    .await;
    match result {
        Ok(completion) => Ok(completion.terminate(session).await),
        Err(error) => Err(error.combine(session.terminate().await)),
    }
}

enum Phase {
    Ready,
    RolledBack,
    Uncertain,
}

fn outcome(state: State) -> &'static str {
    match state {
        State::Matching => "completed",
        State::Empty => "was not committed",
    }
}

#[derive(Error)]
pub enum Error {
    #[error("key initialization requires the schema owner")]
    Identity,
    #[error("the instance fence refused key initialization: {0}")]
    Fence(#[source] domain::runtime::Error),
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
        source: EndError,
    },
    #[error("deployment operation budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("{0}")]
    End(#[from] EndError),
    #[error("key initialization was not committed")]
    NotCommitted(#[source] Box<Error>),
    #[error("key initialization completed but transaction cleanup failed")]
    Completed {
        #[source]
        cause: Box<Error>,
    },
    #[error("key initialization {} but readback cleanup failed", outcome(*.state))]
    Recovered {
        state: State,
        #[source]
        cause: Box<Error>,
        rollback: Result<(), EndError>,
        termination: Result<(), platform::database::Error>,
    },
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
            Self::End(source) => formatter.debug_tuple("End").field(source).finish(),
            Self::Rollback { cause, source } => formatter
                .debug_struct("Rollback")
                .field("cause", cause)
                .field("rollback", source)
                .finish(),
            Self::Completed { cause } => formatter
                .debug_struct("Completed")
                .field("cause", cause)
                .finish(),
            Self::Recovered {
                state,
                cause,
                rollback,
                termination,
            } => formatter
                .debug_struct("Recovered")
                .field("outcome", &outcome(*state))
                .field("cause", cause)
                .field("rollback", rollback)
                .field("termination", termination)
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

    use domain::{
        database::transaction::{EndError, Known},
        keys::database::{self, State},
    };
    use fixture::database_proxy::{Fault, Proxy};
    use platform::database::{Connections, Session, User};
    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        keys::{Error, initialize, reconcile},
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
            diagnostic
                .contains("cause: Metadata(key metadata does not match the loaded root versions)")
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
            initialize(
                &connections,
                &expected,
                crate::tests::fence(&server, &name).await,
            )
            .await
            .expect("initialize");
            initialize(
                &connections,
                &expected,
                crate::tests::fence(&server, &name).await,
            )
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
            for statement in [
                format!("INSERT INTO {TABLE} VALUES (8, decode(repeat('00',32),'hex'), false)"),
                format!("UPDATE {TABLE} SET fingerprint = decode(repeat('00',32),'hex')"),
                format!("UPDATE {TABLE} SET active = false"),
                format!("DELETE FROM {TABLE}"),
                format!("TRUNCATE {TABLE} CASCADE"),
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
            assert!(
                initialize(
                    &connections,
                    &metadata(1, false),
                    crate::tests::fence(&server, &name).await
                )
                .await
                .is_err()
            );
            let observation = server.connections(&name, "steward");
            let session = observation.recover().await.expect("steward");
            database::verify(&expected, session.client())
                .await
                .expect("unchanged after rejection");
            // Python hashlib over LP32("nucleus/identity/key/v1", i64be(0), purpose, bytes 0..31)
            // for two former per-purpose names: only the "root" purpose is accepted.
            for previous in [
                "2c0dfb95b0be5387fd5d04453c6e5e7832a674e463f91a16a28247e77961457f",
                "fe0dbf51999b1070ff403e6c63ef926c964b17caad7dd4d3f3b9c4ce71b8085b",
            ] {
                let original: Vec<u8> = session
                    .client()
                    .query_one(
                        &format!("SELECT fingerprint FROM {TABLE} WHERE id = 0"),
                        &[],
                    )
                    .await
                    .expect("root fingerprint")
                    .get(0);
                session
                    .client()
                    .execute(
                        &format!("UPDATE {TABLE} SET fingerprint = decode($1, 'hex') WHERE id = 0"),
                        &[&previous],
                    )
                    .await
                    .expect("previous-purpose fixture");
                assert!(matches!(
                    initialize(
                        &connections,
                        &expected,
                        crate::tests::fence(&server, &name).await
                    )
                    .await,
                    Err(crate::keys::Error::Metadata(database::Error::Mismatch))
                ));
                let retained: String = session
                    .client()
                    .query_one(
                        &format!("SELECT encode(fingerprint, 'hex') FROM {TABLE} WHERE id = 0"),
                        &[],
                    )
                    .await
                    .expect("read original failed input")
                    .get(0);
                assert_eq!(retained, previous);
                session
                    .client()
                    .execute(
                        &format!("UPDATE {TABLE} SET fingerprint = $1 WHERE id = 0"),
                        &[&original],
                    )
                    .await
                    .expect("restore root fingerprint");
            }
            database::verify(&expected, session.client())
                .await
                .expect("root metadata restored");
            // A second active version is a structural violation, never a later comparison.
            for (statement, state) in [
                (
                    format!(
                        "INSERT INTO {TABLE} VALUES (-1, decode(repeat('00',32),'hex'), false)"
                    ),
                    tokio_postgres::error::SqlState::CHECK_VIOLATION,
                ),
                (
                    format!("INSERT INTO {TABLE} VALUES (9, decode('00','hex'), false)"),
                    tokio_postgres::error::SqlState::CHECK_VIOLATION,
                ),
                (
                    format!("INSERT INTO {TABLE} VALUES (0, decode(repeat('00',32),'hex'), false)"),
                    tokio_postgres::error::SqlState::UNIQUE_VIOLATION,
                ),
                (
                    format!("INSERT INTO {TABLE} VALUES (9, decode(repeat('00',32),'hex'), true)"),
                    tokio_postgres::error::SqlState::UNIQUE_VIOLATION,
                ),
                (
                    format!("UPDATE {TABLE} SET active = true WHERE id = 1"),
                    tokio_postgres::error::SqlState::UNIQUE_VIOLATION,
                ),
                (
                    format!("UPDATE {TABLE} SET active = NULL WHERE id = 1"),
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
            session.terminate().await.expect("end steward");
        });
    }

    #[test]
    fn rejects_missing_inactive_and_extra_versions_without_repairing_them() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            for (statement, count, active) in [
                (format!("DELETE FROM {TABLE} WHERE NOT active"), 1_i64, 1_i64),
                (format!("UPDATE {TABLE} SET active = false"), 2, 0),
                (format!("UPDATE {TABLE} SET active = false WHERE active; UPDATE {TABLE} SET active = true WHERE id = 1"), 2, 1),
                (format!("INSERT INTO {TABLE} VALUES (9, decode(repeat('00',32),'hex'), false)"), 3, 1),
            ] {
                let name = crate::tests::database(&server).await;
                let connections = server.connections(&name, "steward");
                migration::run(&connections, &User::new("clerk".into()).expect("runtime user")).await.expect("migration");
                let expected = metadata(0, false);
                initialize(&connections, &expected, crate::tests::fence(&server, &name).await).await.expect("initialize");
                let session = connections.recover().await.expect("owner");
                session.client().batch_execute(&statement).await.expect("inconsistent fixture");
                session.terminate().await.expect("end fixture session");
                assert!(matches!(initialize(&connections, &expected, crate::tests::fence(&server, &name).await).await,
                    Err(crate::keys::Error::Metadata(database::Error::Mismatch))));
                let runtime = server.connections(&name, "clerk").recover().await.expect("runtime");
                assert!(matches!(database::verify(&expected, runtime.client()).await, Err(database::Error::Mismatch)));
                let row = runtime.client().query_one(&format!("SELECT count(*) AS total, count(*) FILTER (WHERE active) AS active FROM {TABLE}"), &[]).await.expect("unchanged metadata");
                assert_eq!(row.get::<_, i64>("total"), count);
                assert_eq!(row.get::<_, i64>("active"), active);
                runtime.terminate().await.expect("end runtime");
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
            assert!(matches!(
                database::initialize(&expected, &transaction)
                    .await
                    .expect("uncommitted metadata"),
                domain::database::transaction::Outcome::Pending(())
            ));
            drop(transaction);
            session.terminate().await.expect("old driver ended");
            let (Known { value, end }, cleanup) = reconcile(&connections, &expected)
                .await
                .expect("rollback readback");
            assert_eq!(value, State::Empty);
            end.expect("readback rollback");
            cleanup.expect("readback driver");

            let mut session = connections.recover().await.expect("session");
            let transaction = session
                .client_mut()
                .transaction()
                .await
                .expect("transaction");
            assert!(matches!(
                database::initialize(&expected, &transaction)
                    .await
                    .expect("metadata"),
                domain::database::transaction::Outcome::Pending(())
            ));
            // Simulate losing the successful result at the caller boundary, not a wire fault.
            transaction.commit().await.expect("server committed");
            session.terminate().await.expect("old driver ended");
            let (Known { value, end }, cleanup) = reconcile(&connections, &expected)
                .await
                .expect("commit readback");
            assert_eq!(value, State::Matching);
            end.expect("readback rollback");
            cleanup.expect("readback driver");
            let session = connections.recover().await.expect("session");
            session
                .client()
                .batch_execute("UPDATE security.keys SET active = false")
                .await
                .expect("incomplete fixture");
            session.terminate().await.expect("end driver");
            assert!(
                initialize(
                    &connections,
                    &expected,
                    crate::tests::fence(&server, &name).await
                )
                .await
                .is_err()
            );
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
                 CREATE CONSTRAINT TRIGGER reject_commit AFTER INSERT ON security.keys
                 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.reject_commit()"#,
            ).await.expect("commit failure fixture");
            session.terminate().await.expect("end fixture driver");
            let expected = metadata(0, false);
            let error = initialize(&connections, &expected, crate::tests::fence(&server, &name).await).await.expect_err("commit rejected");
            assert!(matches!(error, crate::keys::Error::NotCommitted(_)));
            assert!(!format!("{error:?}").contains("sensitive"));
            let (Known { value, end }, cleanup) = reconcile(&connections, &expected).await.expect("readback");
            assert_eq!(value, State::Empty);
            end.expect("readback rollback");
            cleanup.expect("readback driver");
            assert!(matches!(initialize(&server.connections(&name, "clerk"), &expected, crate::tests::fence(&server, &name).await).await,
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
            // Taken before the first transaction holds the barrier that advancing waits for.
            let fence = crate::tests::fence(&server, &name).await;
            let mut session = first.recover().await.expect("first session");
            let transaction = session
                .client_mut()
                .transaction()
                .await
                .expect("first transaction");
            assert!(matches!(
                database::initialize(&expected, &transaction)
                    .await
                    .expect("first initialization"),
                domain::database::transaction::Outcome::Pending(())
            ));
            let waiter =
                tokio::spawn(async move { initialize(&second, &metadata(0, false), fence).await });
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

    const TABLE: &str = "security.keys";

    async fn prepared(server: &crate::tests::Server) -> (String, Connections, Session) {
        let name = crate::tests::database(server).await;
        let connections = server.connections(&name, "steward");
        migration::run(
            &connections,
            &User::new("clerk".into()).expect("runtime user"),
        )
        .await
        .expect("migrations");
        let observer = server
            .connections(&name, "postgres")
            .recover()
            .await
            .expect("independent observer");
        observer
            .client()
            .batch_execute(
                r#"CREATE SEQUENCE identity.key_writes;
             GRANT USAGE ON SEQUENCE identity.key_writes TO steward;
             CREATE FUNCTION identity.count_key_write() RETURNS trigger LANGUAGE plpgsql AS
             $$ BEGIN PERFORM nextval('identity.key_writes'); RETURN NEW; END $$"#,
            )
            .await
            .expect("nontransactional write counter");
        observer.client().batch_execute(&format!(
            "CREATE TRIGGER count_key_write BEFORE INSERT ON {TABLE} FOR EACH ROW EXECUTE FUNCTION identity.count_key_write()"
        )).await.expect("watch the key table");
        (name, connections, observer)
    }

    async fn reject_commit(observer: &Session) {
        observer.client().batch_execute(
            r#"CREATE FUNCTION identity.reject_key_commit() RETURNS trigger LANGUAGE plpgsql AS
             $$ BEGIN RAISE EXCEPTION 'fictional-sensitive-commit-detail' USING ERRCODE = '23514'; END $$;
             CREATE CONSTRAINT TRIGGER reject_key_commit AFTER INSERT ON security.keys
             DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION identity.reject_key_commit()"#,
        ).await.expect("reject only the isolated key commit");
    }

    async fn snapshot(observer: &Session) -> String {
        observer.client().query_one(&format!(
            "SELECT COALESCE(jsonb_agg(jsonb_build_array(id, encode(fingerprint, 'hex'), active, xmin::text) ORDER BY id)::text, '[]') FROM {TABLE}"
        ), &[]).await.expect("independent retained metadata").get(0)
    }

    async fn assert_registry(observer: &Session, state: State) {
        let row = observer.client().query_one(&format!(
            "SELECT count(*) AS total, count(*) FILTER (WHERE active) AS active FROM {TABLE}"
        ), &[]).await.expect("independent complete registry");
        let expected = if state == State::Matching {
            (2, 1)
        } else {
            (0, 0)
        };
        assert_eq!(
            (row.get::<_, i64>("total"), row.get::<_, i64>("active")),
            expected
        );
        if state == State::Matching {
            database::verify(&metadata(0, false), observer.client())
                .await
                .expect("all fingerprints match");
        }
        let writes: i64 = observer
            .client()
            .query_one("SELECT last_value FROM identity.key_writes", &[])
            .await
            .expect("independent attempted write count")
            .get(0);
        assert_eq!(
            writes, 2,
            "recovery must not repeat even rolled-back INSERTs"
        );
    }

    async fn ended(observer: &Session, name: &str) {
        timeout(Duration::from_secs(5), async {
            loop {
                let ended: bool = observer.client().query_one(
                    "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname = $1 AND usename = 'steward')",
                    &[&name],
                ).await.expect("observe isolated deployment backends").get(0);
                if ended { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("all original and recovery backends ended");
    }

    #[test]
    fn known_readbacks_preserve_initial_failure_and_cleanup_errors() {
        let server = crate::tests::server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("single-thread runtime").block_on(async {
            for (state, expected_outcome) in [(State::Matching, "completed"), (State::Empty, "was not committed")] {
                for fault in [Fault::RejectedRollback, Fault::StalledRollback, Fault::RollbackTermination] {
                    let stalled = matches!(fault, Fault::StalledRollback);
                    let terminated = matches!(fault, Fault::RollbackTermination);
                    let (name, connections, observer) = prepared(&server).await;
                    let proxy = Proxy::start(server.fixture()).await;
                    if state == State::Empty { reject_commit(&observer).await; }
                    if state == State::Matching { proxy.arm(Fault::DelayedKeyCommit); }
                    else { proxy.arm(fault); }
                    let proxied = server.connections_at(&name, "steward", proxy.port());
                    let fence = crate::tests::fence(&server, &name).await;
                    let task = tokio::spawn(async move { initialize(&proxied, &metadata(0, false), fence).await });
                    if state == State::Matching {
                        timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("original COMMIT held");
                        wait_for_lock(&observer, &name).await;
                        // The recovery cannot finish reading before this original transaction ends.
                        let fault = if stalled { Fault::StalledRollback } else if terminated { Fault::RollbackTermination } else { Fault::RejectedRollback };
                        proxy.arm(fault);
                        proxy.release();
                    }
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("readback cleanup reached");
                    if stalled {
                        // Only the response is held: PostgreSQL has already completed ROLLBACK.
                        tokio::time::pause();
                        tokio::time::advance(crate::OPERATION_BUDGET + Duration::from_millis(1)).await;
                        tokio::time::resume();
                    }
                    let error = timeout(Duration::from_secs(5), task).await.expect("bounded cleanup").expect("initializer task").expect_err("known result with cleanup failure");
                    let diagnostic = format!("{error:?}");
                    assert!(diagnostic.contains(expected_outcome), "{diagnostic}");
                    assert!(error.to_string().contains(expected_outcome));
                    assert!(!diagnostic.contains("sensitive"));
                    assert!(!diagnostic.contains(&name));
                    assert!(!diagnostic.contains(fixture::PASSWORD));
                    let Error::Recovered { state: actual, cause, rollback, termination } = error else { panic!("known readback required: {diagnostic}"); };
                    assert_eq!(actual, state);
                    let cause = match *cause {
                        Error::Combined { cause, termination } => {
                            assert!(matches!(termination, platform::database::Error::Postgres(_)));
                            *cause
                        }
                        cause => cause,
                    };
                    let Error::End(EndError::Commit(query)) = cause else { panic!("original initialization COMMIT failure required: {cause:?}"); };
                    if state == State::Matching {
                        let source = std::error::Error::source(&query).and_then(|source| source.downcast_ref::<tokio_postgres::Error>()).expect("actual PostgreSQL transport cause");
                        assert!(source.is_closed());
                    } else {
                        assert_eq!(query.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                    }
                    if stalled {
                        assert!(matches!(rollback, Err(EndError::RollbackTimeout(_))));
                        termination.expect("stalled live driver cancelled");
                        proxy.release();
                    } else if terminated {
                        rollback.expect("server rollback acknowledged");
                        assert!(matches!(termination, Err(platform::database::Error::Postgres(ref source))
                            if source.code() == Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)), "{termination:?}");
                    } else {
                        assert!(matches!(rollback, Err(EndError::Rollback(ref source))
                            if std::error::Error::source(source)
                                .and_then(|source| source.downcast_ref::<tokio_postgres::Error>())
                                .is_some_and(tokio_postgres::Error::is_closed)));
                        assert!(matches!(termination, Err(platform::database::Error::Postgres(_))), "{termination:?}");
                    }
                    ended(&observer, &name).await;
                    assert_registry(&observer, state).await;
                    let before = snapshot(&observer).await;
                    if state == State::Empty {
                        observer.client().batch_execute("DROP TRIGGER reject_key_commit ON security.keys").await.expect("allow separate new attempt");
                    }
                    initialize(&connections, &metadata(0, false), crate::tests::fence(&server, &name).await).await.expect("explicit later retry");
                    if state == State::Matching {
                        assert_eq!(snapshot(&observer).await, before);
                        assert_registry(&observer, state).await;
                    }
                    observer.terminate().await.expect("end observer");
                    connections.close().await;
                    proxy.stop().await;
                }
            }
        });
    }

    #[test]
    fn first_matching_registry_survives_commit_failure_without_another_read() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            let (name, connections, observer) = prepared(&server).await;
            initialize(&connections, &metadata(0, false), crate::tests::fence(&server, &name).await).await.expect("first initialization");
            assert_registry(&observer, State::Matching).await;
            let before = snapshot(&observer).await;
            let proxy = Proxy::start(server.fixture()).await;
            let proxied = server.connections_at(&name, "steward", proxy.port());
            proxy.arm(Fault::StalledReadCommit);
            let fence = crate::tests::fence(&server, &name).await;
            let task = tokio::spawn(async move { initialize(&proxied, &metadata(0, false), fence).await });
            timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("matching registry fully read before held COMMIT response");
            proxy.arm(Fault::RejectedBegin); proxy.release();
            let result = timeout(Duration::from_secs(3), task).await.expect("bounded initialization").expect("task");
            assert_registry(&observer, State::Matching).await;
            assert_eq!(snapshot(&observer).await, before, "the key table is unchanged before any retry");
            let error = result.expect_err("cleanup failure must keep the command unsuccessful");
            assert!(matches!(&error, Error::Completed { cause } if matches!(cause.as_ref(), Error::Combined { cause, termination: platform::database::Error::Postgres(_) } if matches!(cause.as_ref(), Error::End(EndError::Commit(query)) if std::error::Error::source(query).and_then(|source| source.downcast_ref::<tokio_postgres::Error>()).is_some_and(tokio_postgres::Error::is_closed)))), "matching was already known with actual cleanup failure: {error:?}");
            ended(&observer, &name).await;
            observer.terminate().await.expect("observer ended"); connections.close().await; proxy.stop().await;
        });
    }

    #[test]
    fn confirmed_commit_preserves_a_real_driver_failure() {
        let server = crate::tests::server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("single-thread runtime").block_on(async {
            let (name, connections, observer) = prepared(&server).await;
            let proxy = Proxy::start(server.fixture()).await;
            proxy.arm(Fault::CommitTermination);
            let proxied = server.connections_at(&name, "steward", proxy.port());
            let error = timeout(Duration::from_secs(5), initialize(&proxied, &metadata(0, false), crate::tests::fence(&server, &name).await))
                .await.expect("commit budget").expect_err("completed with a driver error");
            timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("real COMMIT response followed by terminal fault");
            assert!(!format!("{error:?}").contains("sensitive"));
            assert!(error.to_string().contains("completed"));
            assert!(matches!(&error, Error::Completed { cause }
                if matches!(cause.as_ref(), Error::Connection(platform::database::Error::Postgres(source))
                    if source.code() == Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE))), "{error:?}");
            ended(&observer, &name).await;
            assert_registry(&observer, State::Matching).await;
            let before = snapshot(&observer).await;
            initialize(&connections, &metadata(0, false), crate::tests::fence(&server, &name).await).await.expect("read-only retry");
            assert_eq!(snapshot(&observer).await, before);
            assert_registry(&observer, State::Matching).await;
            observer.terminate().await.expect("end observer");
            connections.close().await;
            proxy.stop().await;
        });
    }

    #[test]
    fn metadata_rejection_retains_rollback_timeout_and_unknown_readback() {
        let server = crate::tests::server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("single-thread runtime").block_on(async {
            let (name, connections, observer) = prepared(&server).await;
            initialize(&connections, &metadata(0, false), crate::tests::fence(&server, &name).await).await.expect("original registry");
            let before = snapshot(&observer).await;
            assert!(matches!(initialize(&connections, &metadata(1, false), crate::tests::fence(&server, &name).await).await,
                Err(Error::Metadata(database::Error::Mismatch))));
            let proxy = Proxy::start(server.fixture()).await;
            proxy.arm(Fault::RollbackTermination);
            let proxied = server.connections_at(&name, "steward", proxy.port());
            let error = timeout(Duration::from_secs(5), initialize(&proxied, &metadata(1, false), crate::tests::fence(&server, &name).await))
                .await.expect("confirmed refusal rollback").expect_err("known rejection with driver failure");
            timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("refusal ROLLBACK acknowledged");
            assert!(!format!("{error:?}").contains("sensitive"));
            let Error::NotCommitted(cause) = error else { panic!("confirmed rollback remains uncommitted: {error:?}"); };
            assert!(matches!(*cause, Error::Combined { cause, termination: platform::database::Error::Postgres(ref source) }
                if matches!(*cause, Error::Metadata(database::Error::Mismatch))
                    && source.code() == Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)));
            ended(&observer, &name).await;
            assert_eq!(snapshot(&observer).await, before);
            assert_registry(&observer, State::Matching).await;
            proxy.arm(Fault::StalledRollback);
            let proxied = server.connections_at(&name, "steward", proxy.port());
            let fence = crate::tests::fence(&server, &name).await;
            let task = tokio::spawn(async move { initialize(&proxied, &metadata(1, false), fence).await });
            timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("rejected initialization rolled back");
            tokio::time::pause();
            tokio::time::advance(crate::OPERATION_BUDGET + Duration::from_millis(1)).await;
            tokio::time::resume();
            let error = timeout(Duration::from_secs(5), task).await.expect("readback budget").expect("initializer task").expect_err("mismatched readback remains unknown");
            let Error::Unknown { cause, recovery } = error else { panic!("unconfirmed readback required: {error:?}"); };
            assert!(matches!(*cause, Error::Rollback { cause: database::Error::Mismatch, source: EndError::RollbackTimeout(_) }));
            assert!(matches!(*recovery, Error::Metadata(database::Error::Mismatch)));
            assert_eq!(snapshot(&observer).await, before);
            assert_registry(&observer, State::Matching).await;
            proxy.release();
            ended(&observer, &name).await;
            observer.terminate().await.expect("end observer");
            connections.close().await;
            proxy.stop().await;
        });
    }

    #[test]
    fn failed_readback_keeps_the_original_and_recovery_failures_unknown() {
        let server = crate::tests::server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("single-thread runtime").block_on(async {
            let (name, connections, observer) = prepared(&server).await;
            let proxy = Proxy::start(server.fixture()).await;
            proxy.arm(Fault::DelayedKeyCommit);
            let proxied = server.connections_at(&name, "steward", proxy.port());
            let fence = crate::tests::fence(&server, &name).await;
            let task = tokio::spawn(async move { initialize(&proxied, &metadata(0, false), fence).await });
            timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("original COMMIT held");
            wait_for_lock(&observer, &name).await;
            proxy.stop().await;
            let error = timeout(Duration::from_secs(5), task).await.expect("readback fails").expect("initializer task").expect_err("unknown readback");
            let Error::Unknown { cause, recovery } = error else { panic!("no known readback: {error:?}"); };
            assert!(matches!(*cause, Error::Combined { cause, termination: platform::database::Error::Postgres(_) }
                if matches!(*cause, Error::End(EndError::Commit(_)))));
            assert!(matches!(*recovery, Error::Combined { cause, termination: platform::database::Error::Postgres(_) }
                if matches!(*cause, Error::Metadata(database::Error::Database(_)))));
            ended(&observer, &name).await;
            assert_registry(&observer, State::Empty).await;
            observer.terminate().await.expect("end observer");
            connections.close().await;
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
