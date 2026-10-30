//! Offline commands use the same verified application and executor as online work.

mod settings;

use std::{
    fmt,
    io::{self, Write},
    path::PathBuf,
};

use clap::{Args, Subcommand};
use domain::{
    access::{appoint, initialize},
    center::affiliation,
    database::Barrier,
    identity::{correction, operation, website},
    runtime::Context,
    startup::Administration,
};
use platform::database::Connections;

use crate::{access::files, keys};

#[derive(Subcommand)]
pub enum Action {
    Initialize(Options),
    Appoint(Options),
    Correct(Options),
    CorrectAffiliation(Options),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{
        io::{self, Write},
        sync::mpsc,
        time::Duration,
    };

    use domain::{
        access::{appoint, initialize},
        identity::{account, operation, person::revision},
        keys::{Entry, Id, Keys, Material},
    };
    use platform::database::User;
    use serde_json::json;
    use tokio::{runtime::Runtime, sync::oneshot, time::timeout};
    use zeroize::Zeroizing;

    use crate::admin::{Error, Input, execute};

    fn keys() -> Keys {
        let id = Id::new(0).expect("key id");
        Keys::new(
            id,
            vec![Entry::new(
                id,
                Material::new(Zeroizing::new(
                    "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8".into(),
                ))
                .expect("fictional material"),
            )],
        )
        .expect("fixture root")
    }

    fn initialize(name: &str) -> Input {
        Input::Initialize(
            serde_json::from_value(json!({
                "mobile": "13800000000", "name": name, "idNumber": "000000200002290010",
            }))
            .expect("initial identity"),
        )
    }

    fn correct(
        mobile: &str,
        person: domain::identity::person::Id,
        revision: revision::Id,
        name: &str,
        number: &str,
    ) -> Input {
        Input::Correct(
            serde_json::from_value(json!({
                "operator": {"mobile": mobile},
                "target": {"personId": person, "revisionId": revision},
                "name": name,
                "idNumber": number,
            }))
            .expect("system correction input"),
        )
    }

    async fn prepared(server: &crate::tests::Server) -> String {
        let name = crate::tests::database(server).await;
        let connections = server.connections(&name, "steward");
        if let Err(error) = crate::migration::run(
            &connections,
            &User::new("clerk".into()).expect("runtime user"),
        )
        .await
        {
            let mut source: &dyn std::error::Error = &error;
            loop {
                if let Some(error) = source.downcast_ref::<tokio_postgres::Error>()
                    && let Some(error) = error.as_db_error()
                {
                    panic!(
                        "isolated migration: {} {} {:?}",
                        error.code().code(),
                        error.message(),
                        error.position()
                    );
                }
                match source.source() {
                    Some(next) => source = next,
                    None => panic!("isolated migration failed: {error:?}"),
                }
            }
        }
        crate::keys::initialize(
            &connections,
            keys().metadata(),
            crate::tests::fence(server, &name).await,
        )
        .await
        .expect("key metadata");
        connections.close().await;
        name
    }

    async fn run(
        server: &crate::tests::Server,
        name: &str,
        operation: operation::Id,
        input: Input,
        dry_run: bool,
    ) -> Result<String, Error> {
        let mut output = Vec::new();
        execute(
            server.connections(name, "clerk"),
            keys(),
            "website-fixture".parse().expect("AppId"),
            operation,
            input,
            dry_run,
            &mut output,
        )
        .await?;
        assert!(
            crate::tests::released(server, name).await,
            "output precedes instance release"
        );
        Ok(String::from_utf8(output).expect("safe text"))
    }

    async fn snapshot(client: &tokio_postgres::Client) -> Vec<String> {
        let mut rows = Vec::new();
        let tables = client.query(
            "SELECT format('%I.%I', namespace.nspname, relation.relname) FROM pg_catalog.pg_class relation
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid=relation.relnamespace
             WHERE namespace.nspname IN ('identity','access','center','workforce') AND relation.relkind='r'
             ORDER BY namespace.nspname,relation.relname", &[],
        ).await.expect("business tables in the actual migrated database");
        for table in tables {
            let table: String = table.get(0);
            // PostgreSQL quotes catalog identifiers; command input never supplies these names.
            let sql = format!(
                "SELECT COALESCE(jsonb_agg(to_jsonb(record) ORDER BY to_jsonb(record))::text, '[]') FROM {table} record"
            );
            rows.push(
                client
                    .query_one(&sql, &[])
                    .await
                    .expect("independent retained facts")
                    .get(0),
            );
        }
        rows
    }

    #[test]
    fn runtime_admin_initializes_dry_runs_and_replays_original_input() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(40), async {
                let name = prepared(&server).await;
                let reader = server.connections(&name, "clerk").recover().await.expect("observer");
                let before = snapshot(reader.client()).await;
                let operation = uuid::Uuid::new_v4().to_string().parse().expect("operation");
                assert_eq!(reader.client().query_one("SELECT current_user::text", &[])
                    .await.expect("actual runtime identity").get::<_, String>(0), "clerk");
                let mut output = Vec::new();
                let error = execute(
                    server.connections(&name, "steward"), keys(), "website-fixture".parse().expect("AppId"),
                    operation, initialize("Fixture Operator"), false, &mut output,
                ).await.expect_err("the owner cannot substitute for the runtime identity");
                assert!(matches!(error, Error::Barrier(domain::database::Error::Permissions)), "{error:?}");
                assert!(crate::tests::released(&server, &name).await);
                assert_eq!(snapshot(reader.client()).await, before, "wrong identity writes no business facts");
                let output = run(&server, &name, operation, initialize("Fixture Operator"), true)
                    .await.expect("empty database passes full startup and dry run");
                assert!(output.contains("result=dry_run"));
                assert_eq!(snapshot(reader.client()).await, before, "dry run writes no business facts");
                let output = run(&server, &name, operation, initialize("Fixture Operator"), false)
                    .await.expect("runtime initialization");
                assert!(output.contains("result=completed"));
                assert!(output.contains("138****0000"));
                for secret in ["13800000000", "000000200002290010", "Fixture Operator"] {
                    assert!(!output.contains(secret), "stdout contains only masked identity");
                }
                let row = reader.client().query_one(
                    "SELECT (SELECT count(*) FROM identity.accounts), (SELECT count(*) FROM access.roles),
                        (SELECT count(*) FROM access.changes), (SELECT count(*) FROM access.appointments),
                        (SELECT count(*) FROM access.completions), (SELECT count(*) FROM identity.sessions),
                        (SELECT sequence FROM identity.persons),
                        (SELECT sequence FROM access.holders), (SELECT version FROM access.holders)", &[],
                ).await.expect("complete independent initialization graph");
                for (index, expected) in [1_i64,4,4,1,1,0,1,1,1].into_iter().enumerate() {
                    assert_eq!(row.get::<_, i64>(index), expected, "fact {index}");
                }
                let committed = snapshot(reader.client()).await;
                run(&server, &name, operation, initialize("Fixture Operator"), false)
                    .await.expect("original completion precedes nonempty prerequisite");
                assert_eq!(snapshot(reader.client()).await, committed);
                let output = run(&server, &name, operation, initialize("Fixture Operator"), true)
                    .await.expect("dry run finds the committed initialization");
                assert!(output.contains("result=completed"), "{output}");
                assert_eq!(snapshot(reader.client()).await, committed, "known dry run retains the original facts");
                assert!(matches!(
                    run(&server, &name, operation, initialize("Different Operator"), false).await,
                    Err(Error::Initialize(domain::kernel::Error::Mismatch)),
                ));
                assert!(matches!(
                    run(&server, &name, uuid::Uuid::new_v4().to_string().parse().expect("new operation"),
                        initialize("Fixture Operator"), false).await,
                    Err(Error::Initialize(domain::kernel::Error::Rejected(initialize::Rejection::Occupied))),
                ));
                assert_eq!(snapshot(reader.client()).await, committed, "refusals retain all original facts");
                reader.terminate().await.expect("observer ended");
            }).await.expect("runtime initialization budget");
        });
    }

    #[test]
    fn full_startup_rejects_sealed_damage_before_new_work_and_original_replay() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(40), async {
                let name = prepared(&server).await;
                let original = uuid::Uuid::new_v4().to_string().parse().expect("original operation");
                run(&server, &name, original, initialize("Fixture Operator"), false)
                    .await.expect("original initialization");
                let owner = server.connections(&name, "postgres").recover().await.expect("isolated damage injector");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated corruption transaction");
                assert_eq!(owner.client().execute(
                    "UPDATE access.roles SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE scope='workforce' AND level=1", &[],
                ).await.expect("damage an unrelated system role seal"), 1);
                owner.client().batch_execute("COMMIT").await.expect("damage visible before startup");
                owner.terminate().await.expect("damage injector ended");
                let reader = server.connections(&name, "clerk").recover().await.expect("independent runtime observer");
                let damaged = snapshot(reader.client()).await;
                let fresh = uuid::Uuid::new_v4().to_string().parse().expect("new operation");
                for (operation, dry_run) in [(original, false), (fresh, false), (fresh, true)] {
                    let mut output = Vec::new();
                    let error = execute(
                        server.connections(&name, "clerk"), keys(), "website-fixture".parse().expect("AppId"),
                        operation, initialize("Fixture Operator"), dry_run, &mut output,
                    ).await.expect_err("full startup rejects damage before command or replay");
                    let Error::Startup(domain::startup::Error::Role(cause)) = error else {
                        panic!("the startup role stage remains identifiable: {error:?}");
                    };
                    assert!(cause.is_corrupt(), "{cause:?}");
                    assert_eq!(cause.fault().expect("typed verification fault").stage(), domain::kernel::Stage::Command);
                    assert!(String::from_utf8(output).expect("safe text").contains("result=failed"));
                    assert!(crate::tests::released(&server, &name).await);
                    assert_eq!(snapshot(reader.client()).await, damaged, "startup failure never appends business facts");
                }
                reader.terminate().await.expect("observer ended");
            }).await.expect("corrupt startup budget");
        });
    }

    struct FailingOutput {
        flush: bool,
        bytes: Vec<u8>,
        entered: Option<oneshot::Sender<()>>,
        release: Option<mpsc::Receiver<()>>,
    }

    impl Write for FailingOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.flush {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.entered
                .take()
                .expect("flush entered once")
                .send(())
                .expect("flush observer waits");
            self.release
                .take()
                .expect("explicit flush release")
                .recv_timeout(Duration::from_secs(5))
                .expect("flush observer releases output within its deadline");
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn output_failures_keep_committed_results_and_flush_holds_the_runtime_instance() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(60), async {
                for flush in [false, true] {
                    let name = prepared(&server).await;
                    let reader = server.connections(&name, "clerk").recover().await.expect("independent runtime observer");
                    let before = snapshot(reader.client()).await;
                    let operation = uuid::Uuid::new_v4().to_string().parse().expect("operation");
                    let (entered, waiting) = oneshot::channel();
                    let (release, released) = mpsc::channel();
                    let mut output = FailingOutput {
                        flush, bytes: Vec::new(), entered: Some(entered), release: Some(released),
                    };
                    let connections = server.connections(&name, "clerk");
                    let task = tokio::spawn(async move {
                        let result = execute(
                            connections, keys(), "website-fixture".parse().expect("AppId"),
                            operation, initialize("Fixture Operator"), false, &mut output,
                        ).await;
                        (result, output.bytes)
                    });
                    if flush {
                        timeout(Duration::from_secs(20), waiting).await.expect("output reached flush").expect("flush notification");
                        let held = timeout(Duration::from_secs(3), reader.client().query_one(
                            "SELECT activity.usename::text FROM pg_catalog.pg_locks held
                             JOIN pg_catalog.pg_stat_activity activity ON activity.pid=held.pid
                             WHERE held.locktype='advisory' AND held.classid=1314210636 AND held.objid=3
                               AND held.objsubid=2 AND held.granted
                               AND held.database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database())", &[],
                        )).await.expect("bounded independent lock observation").expect("instance is held during flush");
                        assert_eq!(held.get::<_, String>(0), "clerk", "the command actually connects as runtime");
                        assert!(!task.is_finished(), "command waits for the explicitly blocked flush");
                        release.send(()).expect("finish output attempt");
                    }
                    let (result, bytes) = timeout(Duration::from_secs(20), task).await.expect("command finishes after output failure").expect("command task");
                    let error = result.expect_err("output failure fails the command");
                    let Error::Finalization { result, drain, output, termination } = error else {
                        panic!("known business result remains available: {error:?}");
                    };
                    assert_eq!(result.expect("business commit is known"), "completed");
                    assert!(drain.is_none());
                    assert_eq!(output.expect("original output error retained").kind(), io::ErrorKind::BrokenPipe);
                    assert!(termination.is_none());
                    assert!(crate::tests::released(&server, &name).await, "failed output still releases the instance");
                    if flush {
                        assert!(String::from_utf8(bytes).expect("safe output").contains("result=completed"));
                    }
                    let committed = snapshot(reader.client()).await;
                    assert_ne!(committed, before, "the initialization was committed before output failed");
                    run(&server, &name, operation, initialize("Fixture Operator"), false)
                        .await.expect("same operation reads its committed result");
                    assert_eq!(snapshot(reader.client()).await, committed, "output failure does not cause a second write");
                    reader.terminate().await.expect("observer ended");
                }
            }).await.expect("output failure lifecycle budget");
        });
    }

    #[test]
    fn runtime_admin_corrects_dry_runs_name_only_and_replays_original_input() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(60), async {
                let name = prepared(&server).await;
                run(
                    &server,
                    &name,
                    uuid::Uuid::new_v4().to_string().parse().expect("initialization operation"),
                    initialize("Fixture Operator"),
                    false,
                )
                .await
                .expect("initialization");
                let reader = server.connections(&name, "clerk").recover().await.expect("observer");
                let row = reader.client().query_one(
                    "SELECT account.id, account.person_id, person.revision_id, encode(account.id_number,'hex')
                     FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id",
                    &[],
                ).await.expect("initialized subject");
                let account_id: account::Id = row.get(0);
                let person_id: domain::identity::person::Id = row.get(1);
                let revision_id: revision::Id = row.get(2);
                let initial_number: String = row.get(3);
                let before = snapshot(reader.client()).await;
                let operation = uuid::Uuid::new_v4().to_string().parse().expect("correction operation");
                let command = || correct("13800000000", person_id, revision_id, "Fixture Corrected", "000000200002290029");
                let output = run(&server, &name, operation, command(), true).await.expect("real dry-run decision");
                assert!(output.contains("result=dry_run"), "{output}");
                assert!(output.contains("138****0000"), "{output}");
                for secret in ["13800000000", "000000200002290029", "Fixture Corrected"] {
                    assert!(!output.contains(secret), "stdout contains only masked identity");
                }
                assert_eq!(snapshot(reader.client()).await, before, "dry run writes no business facts");
                let output = run(&server, &name, operation, command(), false).await.expect("committed correction");
                assert!(output.contains("result=completed"), "{output}");
                assert!(!output.contains("000000200002290029"), "{output}");
                let committed = snapshot(reader.client()).await;
                assert_ne!(committed, before, "the correction committed permanent facts");
                let facts = reader.client().query_one(
                    "SELECT correction.id, correction.operator_id, correction.appointment_id,
                        completion.family, completion.subject, completion.expires_at IS NULL AS permanent,
                        person.revision_id, person.sequence, encode(account.id_number,'hex'),
                        person.revision_id = correction.id AS current_revision,
                        (SELECT assignment_id FROM access.holders WHERE id=account.id)
                     FROM identity.corrections correction
                     JOIN identity.completions completion
                        ON completion.operation_id=correction.operation_id AND completion.family='system_correction'
                     JOIN identity.persons person ON person.id=correction.person_id
                     JOIN identity.accounts account ON account.person_id=person.id
                     WHERE correction.operation_id=$1",
                    &[&operation],
                ).await;
                let facts = facts.expect("one paired correction and completion");
                assert_eq!(facts.get::<_, revision::Id>(1), revision_id);
                assert_eq!(facts.get::<_, String>(3), "system_correction");
                assert_eq!(facts.get::<_, String>(4), "system");
                assert!(facts.get::<_, bool>(5), "system completions are permanent");
                assert!(facts.get::<_, bool>(9));
                assert_eq!(facts.get::<_, i64>(7), 2);
                assert_ne!(facts.get::<_, String>(8), initial_number);
                assert_eq!(facts.get::<_, domain::access::appointment::Id>(2), facts.get::<_, domain::access::appointment::Id>(10));
                run(&server, &name, operation, command(), false).await.expect("original replay");
                assert_eq!(snapshot(reader.client()).await, committed, "replay writes no facts");
                let changed = correct("13800000000", person_id, revision_id, "Different", "000000200002290029");
                assert!(matches!(
                    run(&server, &name, operation, changed, false).await,
                    Err(Error::Correct(domain::kernel::Error::Mismatch)),
                ));
                assert_eq!(snapshot(reader.client()).await, committed, "mismatch writes no facts");

                let current: revision::Id = reader.client().query_one(
                    "SELECT revision_id FROM identity.persons WHERE id=$1", &[&person_id],
                ).await.expect("current corrected revision").get(0);
                let second = uuid::Uuid::new_v4().to_string().parse().expect("name-only operation");
                run(&server, &name, second, correct("13800000000", person_id, current, "Fixture Renamed", "000000200002290029"), false)
                    .await.expect("name-only correction");
                let row = reader.client().query_one(
                    "SELECT (SELECT count(*) FROM identity.corrections), (SELECT count(*) FROM identity.completions WHERE family='system_correction'),
                        (SELECT count(*) FROM identity.projections WHERE operation_id=$1), encode(account.id_number,'hex')
                     FROM identity.accounts account WHERE account.id=$2",
                    &[&second, &account_id],
                ).await.expect("name-only facts");
                assert_eq!(row.get::<_, i64>(0), 2);
                assert_eq!(row.get::<_, i64>(1), 2);
                assert_eq!(row.get::<_, i64>(2), 0, "a name-only correction does not project onto the account");
                assert_eq!(row.get::<_, String>(3), facts.get::<_, String>(8));
                let after_second = snapshot(reader.client()).await;
                let runtime = server.connections(&name, "clerk").recover().await.expect("runtime writer");
                runtime.client().batch_execute("BEGIN").await.expect("partial group transaction");
                runtime.client().execute(
                    "INSERT INTO identity.completions (family, operation_id, action, target, input, subject, person_id, revision_id, created_at, expires_at, seal, seal_key_id, seal_version)
                     SELECT family, gen_random_uuid(), action, gen_random_uuid()::text, input, subject, person_id, revision_id, created_at, NULL, seal, seal_key_id, seal_version
                     FROM identity.completions WHERE family='system_correction' LIMIT 1",
                    &[],
                ).await.expect("stage an unpaired system completion");
                let error = runtime.client().batch_execute("COMMIT").await.expect_err("a system completion needs its same-transaction correction");
                assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                assert_eq!(
                    error.as_db_error().expect("structured rule").message(),
                    "system correction completion requires its same-transaction correction"
                );
                runtime.client().batch_execute("ROLLBACK").await.expect("discard the unpaired completion");
                runtime.terminate().await.expect("runtime writer ended");
                assert_eq!(snapshot(reader.client()).await, after_second, "the partial group left no facts");
                reader.terminate().await.expect("observer ended");
            }).await.expect("system correction CLI budget");
        });
    }

    #[test]
    fn system_appointments_fix_role_versions_and_replay_after_later_changes() {
        let server = crate::tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(40), async {
                let name = prepared(&server).await;
                run(&server, &name, uuid::Uuid::new_v4().to_string().parse().expect("initial operation"),
                    initialize("Fixture Operator"), false).await.expect("initialization");
                let reader = server.connections(&name, "clerk").recover().await.expect("observer");
                let row = reader.client().query_one(
                    "SELECT account.id, person.revision_id FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id", &[],
                ).await.expect("current target");
                let account_id: account::Id = row.get(0);
                let revision_id: revision::Id = row.get(1);
                let ordinary: domain::access::role::Id = reader.client().query_one(
                    "SELECT id FROM access.roles WHERE scope='institution' AND level=1", &[],
                ).await.expect("ordinary role").get(0);
                let input = |selection| Input::Appoint(serde_json::from_value(json!({
                    "operator": {"mobile": "13800000000"}, "accountId": account_id,
                    "revisionId": revision_id, "selection": selection,
                })).expect("system command"));
                let revoke = uuid::Uuid::new_v4().to_string().parse().expect("revoke operation");
                let selection = appoint::Selection::Revoke { role_id: ordinary };
                let before = snapshot(reader.client()).await;
                let output = run(&server, &name, revoke, input(selection), true).await.expect("revoke dry run");
                assert!(output.contains("result=dry_run"), "{output}");
                assert_eq!(snapshot(reader.client()).await, before);
                run(&server, &name, revoke, input(selection), false).await.expect("revoke self");
                run(&server, &name, uuid::Uuid::new_v4().to_string().parse().expect("grant operation"),
                    input(appoint::Selection::Grant), false).await.expect("grant self");
                let valid: bool = reader.client().query_one(
                    "SELECT holder.sequence=3 AND holder.version=3 AND person.sequence=1
                        AND role.level=1000 AND NOT EXISTS (
                            SELECT 1 FROM access.appointments appointment
                            LEFT JOIN access.changes definition ON definition.id=appointment.change_id
                            WHERE definition.role_id IS DISTINCT FROM appointment.role_id)
                        AND (SELECT count(*) FROM access.completions)=3
                     FROM access.holders holder JOIN identity.persons person ON person.id=holder.person_id
                     JOIN access.roles role ON role.id=holder.role_id WHERE holder.id=$1",
                    &[&account_id],
                ).await.expect("fixed definitions and object-local history").get(0);
                assert!(valid);
                let before = snapshot(reader.client()).await;
                run(&server, &name, revoke, input(selection), false).await.expect("earlier original result");
                assert_eq!(snapshot(reader.client()).await, before, "replay does not restore earlier authority");
                let output = run(&server, &name, revoke, input(selection), true)
                    .await.expect("dry run finds the earlier committed appointment");
                assert!(output.contains("result=completed"), "{output}");
                assert_eq!(snapshot(reader.client()).await, before, "known dry run does not restore earlier authority");
                reader.terminate().await.expect("observer ended");
            }).await.expect("system appointment budget");
        });
    }
}

#[derive(Args)]
pub struct Options {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    operation: operation::Id,
    #[arg(long)]
    dry_run: bool,
}

enum Input {
    Initialize(initialize::Operator),
    Appoint(appoint::Input),
    Correct(correction::admin::Input),
    CorrectAffiliation(affiliation::admin::Input),
}

impl Input {
    fn action(&self) -> &'static str {
        match self {
            Self::Initialize(_) => "initialize",
            Self::Appoint(_) => "appoint",
            Self::Correct(_) => "correct",
            Self::CorrectAffiliation(_) => "correct-affiliation",
        }
    }

    fn masked(&self) -> String {
        match self {
            Self::Initialize(operator) => format!("{} / self", operator.masked()),
            Self::Appoint(input) => input.masked(),
            Self::Correct(input) => input.masked(),
            Self::CorrectAffiliation(input) => input.masked(),
        }
    }

    async fn run(
        self,
        app: &Administration,
        operation: operation::Id,
        dry_run: bool,
    ) -> Result<&'static str, Error> {
        let completed = match self {
            Self::Initialize(input) => app.initialize(operation, input, dry_run).await?.is_some(),
            Self::Appoint(input) => app.appoint(operation, input, dry_run).await?.is_some(),
            Self::Correct(input) => app.correct(operation, input, dry_run).await?.is_some(),
            Self::CorrectAffiliation(input) => app
                .correct_affiliation(operation, input, dry_run)
                .await?
                .is_some(),
        };
        Ok(if completed { "completed" } else { "dry_run" })
    }
}

impl Action {
    pub async fn run(self) -> Result<(), Error> {
        let (options, input) = match self {
            Self::Initialize(options) => {
                let input = files::read(&options.input)?;
                (options, Input::Initialize(input))
            }
            Self::Appoint(options) => {
                let input = files::read(&options.input)?;
                (options, Input::Appoint(input))
            }
            Self::Correct(options) => {
                let input = files::read(&options.input)?;
                (options, Input::Correct(input))
            }
            Self::CorrectAffiliation(options) => {
                let input = files::read(&options.input)?;
                (options, Input::CorrectAffiliation(input))
            }
        };
        let settings = settings::Settings::load()?;
        let keys = keys::credentials::load()?;
        let connections = crate::database::runtime(settings.database)?;
        execute(
            connections,
            keys,
            settings.website,
            options.operation,
            input,
            options.dry_run,
            &mut io::stdout().lock(),
        )
        .await
    }
}

async fn execute(
    connections: Connections,
    keys: domain::keys::Keys,
    website: website::AppId,
    operation: operation::Id,
    input: Input,
    dry_run: bool,
    output: &mut impl Write,
) -> Result<(), Error> {
    let action = input.action();
    let label = input.masked();
    let context = Context::hold(connections).await?;
    let started = async {
        let barrier = Barrier::begin(context.clone()).await?;
        Ok::<_, Error>(Administration::start(barrier, keys, website).await?)
    }
    .await;
    let (result, drain) = match started {
        Ok(app) => {
            let result = match app.confirm().await {
                Ok(()) => input.run(&app, operation, dry_run).await,
                Err(error) => Err(error.into()),
            };
            app.close();
            let drain = app.drain().await.err().map(Box::new);
            (result, drain)
        }
        Err(error) => (Err(error), None),
    };
    let outcome = match &result {
        Ok(outcome) => outcome,
        Err(
            Error::Initialize(domain::kernel::Error::Unknown(_))
            | Error::Appoint(domain::kernel::Error::Unknown(_))
            | Error::Correct(domain::kernel::Error::Unknown(_))
            | Error::Affiliation(domain::kernel::Error::Unknown(_)),
        ) => "outcome_unknown",
        Err(_) => "failed",
    };
    // The instance session remains held through final output, including a failed attempt.
    let output = writeln!(
        output,
        "action={action} subject={label} operation={operation} result={outcome}"
    )
    .and_then(|()| output.flush())
    .err();
    let termination = context.stop().await.err();
    if drain.is_some() || output.is_some() || termination.is_some() {
        return Err(Error::Finalization {
            result: result.map_err(Box::new),
            drain,
            output,
            termination,
        });
    }
    result.map(|_| ())
}

#[derive(thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Input(#[from] crate::access::Error),
    #[error("{0}")]
    Settings(#[from] settings::Error),
    #[error("{0}")]
    Keys(#[from] keys::credentials::Error),
    #[error("{0}")]
    Assembly(#[from] crate::database::Error),
    #[error("{0}")]
    Runtime(#[from] domain::runtime::Error),
    #[error("{0}")]
    Barrier(#[from] domain::database::Error),
    #[error("{0}")]
    Startup(#[from] domain::startup::Error),
    #[error("{0}")]
    Initialize(#[from] initialize::Error),
    #[error("{0}")]
    Appoint(#[from] appoint::Error),
    #[error("{0}")]
    Correct(#[from] correction::admin::Error),
    #[error("{0}")]
    Affiliation(#[from] affiliation::admin::Error),
    #[error("admin finalization failed; the original result is retained")]
    Finalization {
        result: Result<&'static str, Box<Error>>,
        drain: Option<Box<domain::startup::Error>>,
        output: Option<io::Error>,
        termination: Option<domain::runtime::Error>,
    },
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(error) => f.debug_tuple("Input").field(error).finish(),
            Self::Runtime(error) => f.debug_tuple("Runtime").field(error).finish(),
            Self::Barrier(error) => f.debug_tuple("Barrier").field(error).finish(),
            Self::Startup(error) => f.debug_tuple("Startup").field(error).finish(),
            Self::Initialize(error) => f.debug_tuple("Initialize").field(error).finish(),
            Self::Appoint(error) => f.debug_tuple("Appoint").field(error).finish(),
            Self::Correct(error) => f.debug_tuple("Correct").field(error).finish(),
            Self::Affiliation(error) => f.debug_tuple("Affiliation").field(error).finish(),
            Self::Finalization {
                result,
                drain,
                output,
                termination,
            } => f
                .debug_struct("Finalization")
                .field("result", result)
                .field("drain", drain)
                .field("output", &output.as_ref().map(io::Error::kind))
                .field("termination", termination)
                .finish(),
            _ => fmt::Display::fmt(self, f),
        }
    }
}
