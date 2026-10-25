//! Embedded, versioned SQL migrations under the deployment identity.

use std::fmt;

use platform::database::{self, Connections, User};
use refinery::{Migration, Runner};
use thiserror::Error;
use tokio::time::timeout;
use tokio_postgres::Client;

use crate::OPERATION_BUDGET;

/// Every embedded migration as `(name, SQL)` in version order. The online test harnesses apply
/// exactly this list; a test requires it to equal the `migrations` directory.
pub const MIGRATIONS: [(&str, &str); 14] = [
    ("V1__keys", include_str!("../migrations/V1__keys.sql")),
    (
        "V2__challenge_acceptance",
        include_str!("../migrations/V2__challenge_acceptance.sql"),
    ),
    (
        "V3__message_sending",
        include_str!("../migrations/V3__message_sending.sql"),
    ),
    (
        "V4__retention",
        include_str!("../migrations/V4__retention.sql"),
    ),
    (
        "V5__authentication",
        include_str!("../migrations/V5__authentication.sql"),
    ),
    (
        "V6__initialization",
        include_str!("../migrations/V6__initialization.sql"),
    ),
    (
        "V7__registration",
        include_str!("../migrations/V7__registration.sql"),
    ),
    (
        "V8__transfers",
        include_str!("../migrations/V8__transfers.sql"),
    ),
    (
        "V9__affiliations",
        include_str!("../migrations/V9__affiliations.sql"),
    ),
    (
        "V10__invitations",
        include_str!("../migrations/V10__invitations.sql"),
    ),
    (
        "V11__numbers",
        include_str!("../migrations/V11__numbers.sql"),
    ),
    (
        "V12__directories",
        include_str!("../migrations/V12__directories.sql"),
    ),
    (
        "V13__columns",
        include_str!("../migrations/V13__columns.sql"),
    ),
    (
        "V14__avatar_uploads",
        include_str!("../migrations/V14__avatar_uploads.sql"),
    ),
];

fn runner() -> Result<Runner, Error> {
    let mut runner = Runner::new(
        &MIGRATIONS
            .iter()
            .map(|(name, sql)| Migration::unapplied(name, sql))
            .collect::<Result<Vec<_>, _>>()?,
    )
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
        // The session also holds the runtime barrier exclusively, so an application that takes
        // over a lost instance lock waits at its startup barrier until this session ends.
        .batch_execute(
            r#"SELECT pg_catalog.pg_advisory_lock(1314210636, 2);
               SELECT pg_catalog.pg_advisory_lock(1314210636, 1)"#,
        )
        .await?;
    Ok(())
}

/// Apply the embedded migrations, then replace the runtime grants with the manifest.
pub async fn run(connections: &Connections, runtime_user: &User) -> Result<(), Error> {
    let mut session = connections.recover().await?;
    let mut started = false;
    let mut migrated = false;
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
        migrated = true;
        // Keep both session locks until grants finish. A successor cannot advance
        // its generation between the migration commit and grant replacement.
        session
            .client()
            .batch_execute(&domain::database::permissions::grants())
            .await?;
        Ok::<_, Error>(())
    })
    .await;
    let ended = session.terminate().await;
    let cause = match result {
        Ok(Ok(())) => {
            if let Err(termination) = ended {
                return Err(Error::Completed { termination });
            }
            return Ok(());
        }
        Ok(Err(error)) => error,
        Err(error) => error.into(),
    };
    let cause = cause.combine(ended);
    if migrated {
        return Err(Error::Grants(Box::new(cause)));
    }
    if !started {
        return Err(cause);
    }
    match reconcile(connections, runtime_user).await {
        // The original barrier was lost. Readback can establish migration success,
        // but cannot authorize this command to resume writes after a successor.
        Ok((true, Ok(()))) => Err(Error::Grants(Box::new(cause))),
        Ok((true, Err(termination))) => Err(Error::Recovered {
            cause: Box::new(cause),
            termination,
        }),
        Ok((false, termination)) => Err(Error::NotCommitted(Box::new(cause.combine(termination)))),
        Err(recovery) => Err(Error::Unknown {
            cause: Box::new(cause),
            recovery: Box::new(recovery),
        }),
    }
}

async fn reconcile(
    connections: &Connections,
    runtime_user: &User,
) -> Result<(bool, Result<(), database::Error>), Error> {
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
            return Ok((applied, ended));
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
    #[error("migrations completed but the runtime grants were not confirmed")]
    Grants(#[source] Box<Error>),
    #[error("migration was not committed")]
    NotCommitted(#[source] Box<Error>),
    #[error("migration completed but connection termination failed")]
    Completed {
        #[source]
        termination: database::Error,
    },
    #[error("migration completion was recovered but connection termination failed")]
    Recovered {
        #[source]
        cause: Box<Error>,
        termination: database::Error,
    },
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
            Self::Grants(cause) => formatter.debug_tuple("Grants").field(cause).finish(),
            Self::Completed { termination } => formatter
                .debug_struct("Completed")
                .field("termination", termination)
                .finish(),
            Self::Recovered { cause, termination } => formatter
                .debug_struct("Recovered")
                .field("cause", cause)
                .field("termination", termination)
                .finish(),
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

    use fixture::database_proxy::{Fault, Proxy};
    use platform::database::{self, Session, User};
    use tokio::{runtime::Runtime, time::timeout};
    use tokio_postgres::error::SqlState;

    use crate::migration::{Error, lock, reconcile, run, runner};

    #[test]
    fn embedded_migrations_match_the_migration_directory() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut files: Vec<(u32, String, String)> = std::fs::read_dir(&directory)
            .expect("migration directory")
            .map(|entry| {
                let path = entry.expect("migration entry").path();
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("UTF-8 file name")
                    .to_owned();
                let stem = name
                    .strip_suffix(".sql")
                    .unwrap_or_else(|| panic!("unexpected file in migrations: {name}"));
                let version = stem
                    .strip_prefix('V')
                    .and_then(|rest| rest.split_once("__"))
                    .and_then(|(version, _)| version.parse().ok())
                    .unwrap_or_else(|| panic!("unexpected migration name: {name}"));
                let sql = std::fs::read_to_string(&path).expect("migration SQL");
                (version, stem.to_owned(), sql)
            })
            .collect();
        files.sort_unstable_by_key(|(version, _, _)| *version);
        let files: Vec<(String, String)> = files
            .into_iter()
            .map(|(_, name, sql)| (name, sql))
            .collect();
        let embedded: Vec<(String, String)> = crate::migration::MIGRATIONS
            .iter()
            .map(|(name, sql)| ((*name).to_owned(), (*sql).to_owned()))
            .collect();
        assert_eq!(
            files.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            embedded.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            "every migration file is embedded, in version order"
        );
        assert!(files == embedded, "embedded SQL equals the files");
        runner().expect("embedded migrations form a valid runner");
    }

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
    fn migration_session_holds_the_runtime_barrier_until_it_ends() {
        let server = crate::tests::server();
        let user = User::new("clerk".into()).expect("runtime user");
        Runtime::new().expect("runtime").block_on(async {
            let name = crate::tests::database(&server).await;
            let connections = server.connections(&name, "steward");
            let migration = connections.recover().await.expect("migration session");
            lock(migration.client(), &user)
                .await
                .expect("migration locks");
            let probe = server
                .connections(&name, "steward")
                .recover()
                .await
                .expect("probe session");
            let barrier = "BEGIN; SELECT pg_catalog.pg_try_advisory_xact_lock(1314210636, 1)";
            let held: bool = probe
                .client()
                .simple_query(barrier)
                .await
                .expect("barrier probe")
                .into_iter()
                .find_map(|message| match message {
                    tokio_postgres::SimpleQueryMessage::Row(row) => row.get(0).map(|v| v == "t"),
                    _ => None,
                })
                .expect("barrier row");
            assert!(
                !held,
                "a startup barrier must wait for the migration session"
            );
            probe
                .client()
                .batch_execute("ROLLBACK")
                .await
                .expect("end probe");
            migration.terminate().await.expect("end migration session");
            // The server ends the migration backend asynchronously, so wait boundedly for the lock.
            probe
                .client()
                .batch_execute(
                    "SET statement_timeout = '10s'; SELECT pg_catalog.pg_advisory_lock(1314210636, 1)",
                )
                .await
                .expect("the barrier is free once the migration session ends");
            probe.terminate().await.expect("end probe session");
        });
    }

    #[test]
    fn migration_and_grants_keep_one_barrier_session() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let name = crate::tests::database(&server).await;
                let proxy = Proxy::start(server.fixture()).await;
                proxy.arm(Fault::StalledQuery);
                let connections = server.connections_at(&name, "steward", proxy.port());
                let task = tokio::spawn(async move {
                    run(
                        &connections,
                        &User::new("clerk".into()).expect("runtime user"),
                    )
                    .await
                });
                let original = proxy.intercepted().await;
                proxy.arm(Fault::StalledRuntimeGrants);
                proxy.release();
                let granting = proxy.intercepted().await;
                let observer = server
                    .connections(&name, "postgres")
                    .recover()
                    .await
                    .expect("observer");
                let blocked: bool = observer
                    .client()
                    .query_one(
                        "SELECT NOT pg_catalog.pg_try_advisory_lock(1314210636, 1)",
                        &[],
                    )
                    .await
                    .expect("startup barrier probe")
                    .get(0);
                proxy.release();
                task.await
                    .expect("migration task")
                    .expect("migration and grants");
                observer.terminate().await.expect("observer ended");
                proxy.stop().await;
                assert_eq!(
                    granting, original,
                    "the migration barrier session must never end before grants"
                );
                assert!(
                    blocked,
                    "a successor cannot advance while grants are pending"
                );
            })
            .await
            .expect("continuous barrier budget");
        });
    }

    #[test]
    fn recovered_migration_does_not_resume_grants_after_losing_its_barrier() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let name = crate::tests::database(&server).await;
                let proxy = Proxy::start(server.fixture()).await;
                proxy.arm(Fault::LostCommit);
                let connections = server.connections_at(&name, "steward", proxy.port());
                let task = tokio::spawn(async move {
                    run(
                        &connections,
                        &User::new("clerk".into()).expect("runtime user"),
                    )
                    .await
                });
                proxy.intercepted().await;
                proxy.release();
                let error = task
                    .await
                    .expect("migration task")
                    .expect_err("grants require a later explicit run");
                assert!(matches!(error, Error::Grants(_)), "{error:?}");
                let observer = server
                    .connections(&name, "postgres")
                    .recover()
                    .await
                    .expect("observer");
                snapshot(&observer, true).await;
                let granted: bool = observer
                    .client()
                    .query_one(
                        "SELECT has_schema_privilege('clerk', 'center', 'USAGE')",
                        &[],
                    )
                    .await
                    .expect("no grant was resumed")
                    .get(0);
                assert!(!granted);
                observer.terminate().await.expect("observer ended");
                run(
                    &server.connections(&name, "steward"),
                    &User::new("clerk".into()).expect("runtime user"),
                )
                .await
                .expect("explicit rerun completes grants");
                proxy.stop().await;
            })
            .await
            .expect("recovered migration budget");
        });
    }

    #[test]
    fn migration_is_repeatable_and_rejects_changed_history() {
        let server = crate::tests::server();
        let user = User::new("clerk".into()).expect("runtime user");
        Runtime::new().expect("runtime").block_on(async {
            let name = crate::tests::database(&server).await;
            let connections = server.connections(&name, "steward");
            let (applied, ended) = reconcile(&connections, &user)
                .await
                .expect("empty database");
            assert!(!applied);
            ended.expect("end empty readback");
            run(&connections, &user).await.expect("first migration");
            run(&connections, &user).await.expect("repeat migration");
            let other = server.connections(&name, "steward");
            let (first, second) = tokio::join!(run(&connections, &user), run(&other, &user));
            first.expect("first concurrent runner");
            second.expect("second concurrent runner");
            let (applied, ended) = reconcile(&connections, &user)
                .await
                .expect("read committed history");
            assert!(applied);
            ended.expect("end complete readback");
            let mut session = connections.recover().await.expect("session");
            let applied = runner()
                .expect("embedded migration")
                .get_applied_migrations_async(session.client_mut())
                .await
                .expect("history");
            assert_eq!(applied.len(), 14);
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
            let (applied, ended) = reconcile(&connections, &user)
                .await
                .expect("no migration committed");
            assert!(!applied);
            ended.expect("end failed migration readback");
            let session = connections.recover().await.expect("session");
            let absent: bool = session
                .client()
                .query_one(
                    r#"SELECT pg_catalog.to_regclass('security.keys') IS NULL"#,
                    &[],
                )
                .await
                .expect("catalog")
                .get(0);
            assert!(absent);
            session.terminate().await.expect("end fixture driver");
        });
    }

    async fn ended(observer: &Session, name: &str) {
        timeout(Duration::from_secs(5), async {
            loop {
                let ended: bool = observer.client().query_one(
                    "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname = $1 AND usename = 'steward')",
                    &[&name],
                ).await.expect("observe deployment backends").get(0);
                if ended { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("original and recovery backends ended");
    }

    async fn snapshot(observer: &Session, applied: bool) -> Vec<String> {
        let rows = observer.client().query(
            "SELECT version, name, applied_on, checksum, xmin::text AS transaction_id FROM deployment.migrations ORDER BY version",
            &[],
        ).await.expect("independent complete migration history");
        let expected = runner().expect("embedded migrations");
        assert_eq!(rows.len(), if applied { 14 } else { 0 });
        let mut snapshot = Vec::new();
        for (row, migration) in rows.iter().zip(expected.get_migrations()) {
            assert_eq!(row.get::<_, i32>("version"), migration.version());
            assert_eq!(row.get::<_, String>("name"), migration.name());
            assert_eq!(
                row.get::<_, String>("checksum"),
                migration.checksum().to_string()
            );
            snapshot.push(format!(
                "{}:{}:{}:{}:{}",
                row.get::<_, i32>("version"),
                row.get::<_, String>("name"),
                row.get::<_, String>("applied_on"),
                row.get::<_, String>("checksum"),
                row.get::<_, String>("transaction_id")
            ));
        }
        for relation in [
            "security.keys",
            "identity.challenges",
            "identity.messages",
            "identity.claims",
            "identity.sessions",
            "access.holders",
            "access.appointments",
            "access.projections",
            "access.completions",
            "identity.attempts",
        ] {
            let option: Option<u32> = observer
                .client()
                .query_one("SELECT pg_catalog.to_regclass($1)::oid", &[&relation])
                .await
                .expect("independent resulting structure")
                .get(0);
            assert_eq!(option.is_some(), applied, "{relation}");
            if let Some(oid) = option {
                snapshot.push(oid.to_string());
            }
        }
        snapshot
    }

    fn migration_failure(error: &Error) {
        match error {
            Error::Migration(_) => {}
            Error::Combined { cause, termination } => {
                assert!(matches!(**cause, Error::Migration(_)), "{error:?}");
                assert!(
                    matches!(termination, database::Error::Postgres(_)),
                    "{error:?}"
                );
            }
            _ => panic!("the original migration failure must survive: {error:?}"),
        }
    }

    fn terminal_failure(error: &database::Error) {
        assert!(
            matches!(error, database::Error::Postgres(source)
            if source.code() == Some(&SqlState::CONNECTION_FAILURE)),
            "{error:?}"
        );
    }

    #[test]
    fn confirmed_migration_preserves_a_real_driver_failure() {
        let server = crate::tests::server();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("single-thread runtime")
            .block_on(async {
                let name = crate::tests::database(&server).await;
                let user = User::new("clerk".into()).expect("runtime user");
                let observer = server
                    .connections(&name, "postgres")
                    .recover()
                    .await
                    .expect("observer");
                let proxy = Proxy::start(server.fixture()).await;
                proxy.arm(Fault::MigrationCommitTermination);
                let connections = server.connections_at(&name, "steward", proxy.port());
                let error = timeout(Duration::from_secs(10), run(&connections, &user))
                    .await
                    .expect("migration budget")
                    .expect_err("confirmed migration with termination failure");
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("grouped COMMIT response fault");
                assert!(!format!("{error:?}").contains("sensitive"));
                assert!(error.to_string().contains("completed"));
                let Error::Grants(cause) = error else {
                    panic!("completed migration but unconfirmed grants: {error:?}");
                };
                let Error::Combined { cause, termination } = *cause else {
                    panic!("grant and driver failure: {cause:?}");
                };
                assert!(matches!(*cause, Error::Database(_)));
                terminal_failure(&termination);
                ended(&observer, &name).await;
                let before = snapshot(&observer, true).await;
                run(&connections, &user).await.expect("verified repeat");
                ended(&observer, &name).await;
                assert_eq!(snapshot(&observer, true).await, before);
                observer.terminate().await.expect("end observer");
                connections.close().await;
                proxy.stop().await;
            });
    }

    #[test]
    fn migration_readback_keeps_known_outcomes_after_driver_failure() {
        let server = crate::tests::server();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("single-thread runtime")
            .block_on(async {
                for applied in [true, false] {
                    let name = crate::tests::database(&server).await;
                    let observer = server
                        .connections(&name, "postgres")
                        .recover()
                        .await
                        .expect("observer");
                    let proxy = Proxy::start(server.fixture()).await;
                    proxy.arm(if applied {
                        Fault::LostCommit
                    } else {
                        Fault::DelayedRollback
                    });
                    let connections = server.connections_at(&name, "steward", proxy.port());
                    let task = tokio::spawn(async move {
                        run(
                            &connections,
                            &User::new("clerk".into()).expect("runtime user"),
                        )
                        .await
                    });
                    timeout(Duration::from_secs(5), proxy.intercepted())
                        .await
                        .expect("original grouped COMMIT intercepted");
                    if applied {
                        timeout(Duration::from_secs(5), async {
                            loop {
                                let count: i64 = observer
                                    .client()
                                    .query_one("SELECT count(*) FROM deployment.migrations", &[])
                                    .await
                                    .expect("observe original commit")
                                    .get(0);
                                if count == 14 {
                                    break;
                                }
                                tokio::task::yield_now().await;
                            }
                        })
                        .await
                        .expect("original grouped commit reached PostgreSQL");
                    }
                    let before = snapshot(&observer, applied).await;
                    // The original backend still owns the migration lock. Recovery cannot
                    // reach its own history COMMIT before this next fault has been armed.
                    proxy.arm(Fault::CommitTermination);
                    proxy.release();
                    let error = timeout(Duration::from_secs(10), task)
                        .await
                        .expect("readback budget")
                        .expect("migration task")
                        .expect_err("known outcome with failed termination");
                    timeout(Duration::from_secs(5), proxy.intercepted())
                        .await
                        .expect("readback COMMIT succeeded before terminal fault");
                    assert!(!format!("{error:?}").contains("sensitive"));
                    if applied {
                        let Error::Recovered { cause, termination } = error else {
                            panic!("known completion: {error:?}");
                        };
                        migration_failure(&cause);
                        terminal_failure(&termination);
                    } else {
                        let Error::NotCommitted(cause) = error else {
                            panic!("known rollback: {error:?}");
                        };
                        let Error::Combined { cause, termination } = *cause else {
                            panic!("retained termination: {cause:?}");
                        };
                        migration_failure(&cause);
                        terminal_failure(&termination);
                    }
                    ended(&observer, &name).await;
                    assert_eq!(snapshot(&observer, applied).await, before);
                    let connections = server.connections(&name, "steward");
                    run(
                        &connections,
                        &User::new("clerk".into()).expect("runtime user"),
                    )
                    .await
                    .expect("later explicit migration");
                    ended(&observer, &name).await;
                    let after = snapshot(&observer, true).await;
                    if applied {
                        assert_eq!(after, before);
                    }
                    observer.terminate().await.expect("end observer");
                    connections.close().await;
                    proxy.stop().await;
                }
            });
    }

    #[test]
    fn migration_history_commit_failure_remains_unknown() {
        let server = crate::tests::server();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("single-thread runtime")
            .block_on(async {
                let name = crate::tests::database(&server).await;
                let observer = server
                    .connections(&name, "postgres")
                    .recover()
                    .await
                    .expect("observer");
                let proxy = Proxy::start(server.fixture()).await;
                proxy.arm(Fault::LostCommit);
                let connections = server.connections_at(&name, "steward", proxy.port());
                let task = tokio::spawn(async move {
                    run(
                        &connections,
                        &User::new("clerk".into()).expect("runtime user"),
                    )
                    .await
                });
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("original grouped commit intercepted");
                proxy.arm(Fault::LostMigrationReadCommit);
                proxy.release();
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("history rows read but their COMMIT response lost");
                let before = snapshot(&observer, true).await;
                let error = timeout(Duration::from_secs(5), task)
                    .await
                    .expect("unknown readback budget")
                    .expect("migration task")
                    .expect_err("library never returned committed history");
                let Error::Unknown { cause, recovery } = error else {
                    panic!("history API did not complete: {error:?}");
                };
                migration_failure(&cause);
                migration_failure(&recovery);
                proxy.release();
                ended(&observer, &name).await;
                assert_eq!(snapshot(&observer, true).await, before);
                let connections = server.connections(&name, "steward");
                run(
                    &connections,
                    &User::new("clerk".into()).expect("runtime user"),
                )
                .await
                .expect("later explicit readback");
                assert_eq!(snapshot(&observer, true).await, before);
                observer.terminate().await.expect("end observer");
                connections.close().await;
                proxy.stop().await;
            });
    }
}
