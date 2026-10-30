//! Exercise the installed command surface against an isolated PostgreSQL server.

#![allow(clippy::expect_used)]

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use domain::{
    center::affiliation::{self, Code},
    keys::{Entry as RootEntry, Id as KeyId, Keys as RootKeys, Material},
};
use platform::database::{
    Connections, assembly,
    credentials::Password,
    settings::{Budgets, Settings},
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{process::Command, runtime::Runtime, time::timeout};
use uuid::Uuid;
use zeroize::Zeroizing;

const MATERIAL: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
const MOBILE: &str = "13800000000";
const NUMBER: &str = "000000200002290010";

struct Database {
    server: fixture::Server,
    name: String,
    directory: TempDir,
}

impl Database {
    fn connections(&self, name: &str, user: &str) -> Connections {
        Connections::new(
            Settings::new(
                "127.0.0.1".into(),
                self.server.port(),
                name.into(),
                user.into(),
                2,
                Budgets::default(),
            )
            .expect("fixture connection settings"),
            Password::new(fixture::PASSWORD.into()).expect("fictional password"),
            assembly::roots(&fs::read(self.server.authority()).expect("fixture authority"))
                .expect("fixture trust roots"),
        )
        .expect("fixture connections")
    }

    async fn prepare(&self) {
        let session = self
            .connections("postgres", "postgres")
            .recover()
            .await
            .expect("fixture administrator");
        for user in ["cli_migrator", "cli_runtime"] {
            let sql: String = session
                .client()
                .query_one(
                    "SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', $1::text, $2::text)",
                    &[&user, &fixture::PASSWORD],
                )
                .await
                .expect("quote fictional role")
                .get(0);
            session
                .client()
                .batch_execute(&sql)
                .await
                .expect("create fixture role");
        }
        // The database identifier consists only of a fixed prefix and UUID hex.
        session
            .client()
            .batch_execute(&format!("CREATE DATABASE {}", self.name))
            .await
            .expect("isolated database");
        session.terminate().await.expect("end cluster preparation");
        let session = self
            .connections(&self.name, "postgres")
            .recover()
            .await
            .expect("database preparation");
        session.client().query_one(
            "SELECT set_config('nucleus.deployment_user', 'cli_migrator', false), set_config('nucleus.runtime_user', 'cli_runtime', false)", &[],
        ).await.expect("bind isolated roles");
        session
            .client()
            .batch_execute(include_str!("../roles.sql"))
            .await
            .expect("check isolated roles");
        session
            .client()
            .batch_execute(include_str!("../prepare.sql"))
            .await
            .expect("prepare isolated permissions");
        session.terminate().await.expect("end preparation");
        for filename in ["migration-password", "database.json"] {
            self.private(
                filename,
                &json!({"version": 1, "password": fixture::PASSWORD}),
            );
        }
        self.private(
            "keys.json",
            &json!({"version": 1, "active": 0, "keys": [{"id": 0, "material": MATERIAL}]}),
        );
        self.run(self.command(&["migrate"])).await.succeeds(None);
        self.run(self.command(&["keys", "init"]))
            .await
            .succeeds(None);
    }

    fn private(&self, name: &str, value: &Value) -> PathBuf {
        let path = self.directory.path().join(name);
        fs::write(&path, serde_json::to_vec(value).expect("fixture JSON")).expect("fixture file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private input mode");
        path
    }

    fn command(&self, args: &[&str]) -> Command {
        let migration = matches!(args.first(), Some(&"migrate" | &"keys"));
        let prefix = if migration {
            "NUCLEUS_MIGRATION"
        } else {
            "NUCLEUS_DATABASE"
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_deployment"));
        command
            .args(args)
            .env_clear()
            .current_dir(self.directory.path())
            .env("CREDENTIALS_DIRECTORY", self.directory.path())
            .env(format!("{prefix}__HOST"), "127.0.0.1")
            .env(format!("{prefix}__PORT"), self.server.port().to_string())
            .env(format!("{prefix}__NAME"), &self.name)
            .env(format!("{prefix}__AUTHORITY"), self.server.authority())
            .env(
                format!("{prefix}__USER"),
                if migration {
                    "cli_migrator"
                } else {
                    "cli_runtime"
                },
            )
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if migration {
            command.env("NUCLEUS_MIGRATION__RUNTIME_USER", "cli_runtime");
        } else {
            command
                .env("NUCLEUS_DATABASE__CAPACITY", "2")
                .env("NUCLEUS_WECHAT__WEBSITE__APP_ID", "cli-website-fixture");
        }
        command
    }

    fn admin(&self, action: &str, input: &Path, operation: &str, dry_run: bool) -> Command {
        let mut command = self.command(&["admin", action]);
        command
            .arg("--input")
            .arg(input)
            .arg("--operation")
            .arg(operation);
        if dry_run {
            command.arg("--dry-run");
        }
        command
    }

    async fn run(&self, mut command: Command) -> Output {
        let id = Uuid::new_v4();
        let stdout = self.directory.path().join(format!("{id}.stdout"));
        let stderr = self.directory.path().join(format!("{id}.stderr"));
        command
            .stdout(fs::File::create(&stdout).expect("capture stdout"))
            .stderr(fs::File::create(&stderr).expect("capture stderr"));
        let mut child = command.spawn().expect("actual deployment binary");
        let status = match timeout(Duration::from_secs(30), child.wait()).await {
            Ok(status) => status.expect("CLI exit status"),
            Err(_) => {
                child.kill().await.expect("terminate timed out CLI");
                child.wait().await.expect("reap timed out CLI");
                panic!("CLI exceeded its isolated test deadline");
            }
        };
        let output = Output {
            status,
            stdout: fs::read_to_string(stdout).expect("captured stdout"),
            stderr: fs::read_to_string(stderr).expect("captured stderr"),
        };
        for secret in [
            MOBILE,
            NUMBER,
            MATERIAL,
            "CLI Initial",
            "CLI Corrected",
            "CLI Different",
        ] {
            assert!(
                !output.stdout.contains(secret) && !output.stderr.contains(secret),
                "CLI disclosed fictional identity or key material"
            );
        }
        output
    }
}

struct Output {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl Output {
    fn succeeds(&self, result: Option<&str>) {
        assert!(
            self.status.success(),
            "CLI failed: {} {}",
            self.stdout,
            self.stderr
        );
        if let Some(result) = result {
            assert!(self.stdout.contains(result), "{}", self.stdout);
        }
    }

    fn fails(&self) {
        assert!(
            !self.status.success(),
            "CLI unexpectedly succeeded: {}",
            self.stdout
        );
    }
}

async fn snapshot(client: &tokio_postgres::Client) -> Vec<String> {
    let tables = client.query(
        "SELECT format('%I.%I', namespace.nspname, relation.relname) FROM pg_catalog.pg_class relation
         JOIN pg_catalog.pg_namespace namespace ON namespace.oid=relation.relnamespace
         WHERE namespace.nspname IN ('identity','access','center','workforce') AND relation.relkind='r'
         ORDER BY namespace.nspname,relation.relname", &[],
    ).await.expect("catalog business tables");
    let mut result = Vec::new();
    for table in tables {
        let table: String = table.get(0);
        // The database quotes catalog identifiers; no command input supplies SQL.
        let sql = format!(
            "SELECT COALESCE(jsonb_agg(to_jsonb(record) ORDER BY to_jsonb(record))::text, '[]') FROM {table} record"
        );
        result.push(
            client
                .query_one(&sql, &[])
                .await
                .expect("independent business snapshot")
                .get(0),
        );
    }
    result
}

#[test]
fn correct_commits_and_replays_through_the_actual_binary() {
    let database = Database {
        server: fixture::Server::start(),
        name: format!("test_{}", Uuid::new_v4().simple()),
        directory: TempDir::new().expect("private command directory"),
    };
    Runtime::new().expect("test runtime").block_on(async {
        database.prepare().await;
        let initial = database.private("initialize.json", &json!({"mobile": MOBILE, "name": "CLI Initial", "idNumber": NUMBER}));
        database.run(database.admin("initialize", &initial, &Uuid::new_v4().to_string(), false))
            .await.succeeds(Some("result=completed"));
        let observer = database.connections(&database.name, "cli_runtime").recover().await.expect("independent observer");
        let client = observer.client();
        let row = client.query_one(
            "SELECT person.id::text, person.revision_id::text, account.sequence FROM identity.persons person
             JOIN identity.accounts account ON account.person_id=person.id", &[],
        ).await.expect("initialized identity");
        let person: String = row.get(0);
        let revision: String = row.get(1);
        let sequence: i64 = row.get(2);
        let input = json!({"operator": {"mobile": MOBILE}, "target": {"personId": person, "revisionId": revision}, "name": "CLI Corrected", "idNumber": NUMBER});
        let path = database.private("correct.json", &input);
        let operation = Uuid::new_v4().to_string();
        let before = snapshot(client).await;
        database.run(database.admin("correct", &path, &operation, true)).await.succeeds(Some("result=dry_run"));
        assert_eq!(snapshot(client).await, before, "dry run writes no business facts");

        let mut missing_website = database.admin("correct", &path, &operation, false);
        missing_website.env_remove("NUCLEUS_WECHAT__WEBSITE__APP_ID");
        database.run(missing_website).await.fails();
        let mut wrong_user = database.admin("correct", &path, &operation, false);
        wrong_user.env("NUCLEUS_DATABASE__USER", "cli_migrator");
        database.run(wrong_user).await.fails();
        assert_eq!(snapshot(client).await, before, "invalid runtime assembly writes no business facts");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("unsafe mode");
        database.run(database.admin("correct", &path, &operation, false)).await.fails();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore private mode");
        let link = database.directory.path().join("symlink.json");
        symlink(&path, &link).expect("symlink fixture");
        database.run(database.admin("correct", &link, &operation, false)).await.fails();
        let hardlink = database.directory.path().join("hardlink.json");
        fs::hard_link(&path, &hardlink).expect("hardlink fixture");
        database.run(database.admin("correct", &path, &operation, false)).await.fails();
        fs::remove_file(hardlink).expect("restore unique input link");
        let fifo = database.directory.path().join("fifo.json");
        assert!(Command::new("/usr/bin/mkfifo").arg(&fifo).status().await.expect("fixture FIFO").success());
        database.run(database.admin("correct", &fifo, &operation, false)).await.fails();
        database.run(database.command(&["operate-access", "correct", "inspect"])).await.fails();
        assert_eq!(snapshot(client).await, before, "invalid inputs cannot write business facts");

        database.run(database.admin("correct", &path, &operation, false)).await.succeeds(Some("result=completed"));
        // Observe the original commit before any replay could repair a missing effect.
        let facts = client.query_one(
            "SELECT correction.operator_id::text, person.revision_id=correction.id, person.sequence,
                    completion.subject, completion.expires_at IS NULL, completion.revision_id=correction.operator_id,
                    completion.created_at=correction.corrected_at, account.sequence,
                    (SELECT count(*) FROM identity.corrections),
                    (SELECT count(*) FROM identity.completions WHERE family='system_correction')
             FROM identity.corrections correction JOIN identity.persons person ON person.id=correction.person_id
             JOIN identity.accounts account ON account.person_id=person.id
             JOIN identity.completions completion ON completion.operation_id=correction.operation_id AND completion.family='system_correction'
             WHERE correction.operation_id=$1::text::uuid", &[&operation],
        ).await.expect("paired committed correction and permanent completion");
        assert_eq!(facts.get::<_, String>(0), revision);
        assert!(facts.get::<_, bool>(1));
        assert_eq!(facts.get::<_, i64>(2), 2);
        assert_eq!(facts.get::<_, String>(3), "system");
        for column in [4, 5, 6] { assert!(facts.get::<_, bool>(column)); }
        assert_eq!(facts.get::<_, i64>(7), sequence, "name-only correction preserves account projection");
        assert_eq!(facts.get::<_, i64>(8), 1);
        assert_eq!(facts.get::<_, i64>(9), 1);
        let committed = snapshot(client).await;
        assert_ne!(committed, before);
        for dry_run in [false, true] {
            database.run(database.admin("correct", &path, &operation, dry_run)).await.succeeds(Some("result=completed"));
            assert_eq!(snapshot(client).await, committed, "original replay is read-only");
        }
        let mut changed = input;
        changed["name"] = json!("CLI Different");
        let changed = database.private("changed.json", &changed);
        database.run(database.admin("correct", &changed, &operation, false)).await.fails();
        assert_eq!(snapshot(client).await, committed, "mismatched retry cannot change original result");
        observer.terminate().await.expect("end independent observer");
    });
}

fn root_keys() -> RootKeys {
    let id = KeyId::new(0).expect("fixture key id");
    RootKeys::new(
        id,
        vec![RootEntry::new(
            id,
            Material::new(Zeroizing::new(MATERIAL.into())).expect("fictional material"),
        )],
    )
    .expect("fixture root")
}

/// Seed one legitimate first value through the production encoding.
async fn seed(database: &Database) -> (String, String, String, String) {
    let observer = database
        .connections(&database.name, "cli_runtime")
        .recover()
        .await
        .expect("initial identity observer");
    let row = observer
        .client()
        .query_one(
            "SELECT account.id::text, account.person_id::text, person.revision_id::text
             FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id",
            &[],
        )
        .await
        .expect("initialized subject");
    let account: String = row.get(0);
    let person: String = row.get(1);
    let revision: String = row.get(2);
    observer.terminate().await.expect("observer ended");
    let writer = database
        .connections(&database.name, "cli_runtime")
        .recover()
        .await
        .expect("fixture writer");
    writer
        .client()
        .batch_execute("BEGIN")
        .await
        .expect("fixture transaction");
    let operation = Uuid::new_v4()
        .to_string()
        .parse()
        .expect("fixture operation");
    affiliation::seed(
        writer.client(),
        &root_keys(),
        person.parse().expect("fixture person"),
        revision.parse().expect("fixture revision"),
        Code::CommunistParty,
        operation,
    )
    .await
    .expect("seeded first value");
    writer
        .client()
        .batch_execute("COMMIT")
        .await
        .expect("fixture commit");
    let affiliation: String = writer
        .client()
        .query_one(
            "SELECT affiliation_id::text FROM center.profiles WHERE id::text=$1",
            &[&person],
        )
        .await
        .expect("seeded profile")
        .get(0);
    writer.terminate().await.expect("fixture writer ended");
    (account, person, revision, affiliation)
}

#[test]
fn correct_affiliation_commits_and_replays_through_the_actual_binary() {
    let database = Database {
        server: fixture::Server::start(),
        name: format!("test_{}", Uuid::new_v4().simple()),
        directory: TempDir::new().expect("private command directory"),
    };
    Runtime::new().expect("test runtime").block_on(async {
        database.prepare().await;
        let initial = database.private(
            "initialize.json",
            &json!({"mobile": MOBILE, "name": "CLI Initial", "idNumber": NUMBER}),
        );
        database
            .run(database.admin("initialize", &initial, &Uuid::new_v4().to_string(), false))
            .await
            .succeeds(Some("result=completed"));
        let (account, person, revision, affiliation) = seed(&database).await;
        let input = json!({
            "operator": {"mobile": MOBILE, "accountId": account, "revisionId": revision},
            "target": {
                "accountId": account,
                "personId": person,
                "revisionId": revision,
                "affiliationId": affiliation,
            },
            "affiliation": "13",
        });
        let path = database.private("affiliation.json", &input);
        let operation = Uuid::new_v4().to_string();
        let observer = database
            .connections(&database.name, "cli_runtime")
            .recover()
            .await
            .expect("independent observer");
        let before = snapshot(observer.client()).await;

        database
            .run(database.admin("correct-affiliation", &path, &operation, true))
            .await
            .succeeds(Some("result=dry_run"));
        assert_eq!(snapshot(observer.client()).await, before, "dry run writes no business facts");

        let held = domain::database::instance::Instance::hold(
            &database.connections(&database.name, "cli_runtime"),
        )
        .await
        .expect("independent instance lock");
        database
            .run(database.admin("correct-affiliation", &path, &operation, false))
            .await
            .fails();
        held.release().await.expect("instance lock released");
        assert_eq!(snapshot(observer.client()).await, before, "instance exclusion writes no facts");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("unsafe mode");
        database
            .run(database.admin("correct-affiliation", &path, &operation, false))
            .await
            .fails();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore private mode");
        assert_eq!(snapshot(observer.client()).await, before, "unsafe input writes no facts");

        let output = database
            .run(database.admin("correct-affiliation", &path, &operation, false))
            .await;
        output.succeeds(Some("result=completed"));
        assert!(output.stdout.contains("action=correct-affiliation"));
        assert!(!output.stdout.contains(MOBILE));
        assert!(!output.stdout.contains(r#""affiliation":"13""#));
        let committed = snapshot(observer.client()).await;
        assert_ne!(committed, before);
        let facts = observer
            .client()
            .query_one(
                "SELECT person.revision_id=affiliation.operator_id, profile.affiliation_id=affiliation.id,
                        completion.subject, completion.action, completion.expires_at IS NULL,
                        completion.target=affiliation.person_id::text,
                        completion.created_at=affiliation.created_at,
                        (SELECT count(*) FROM center.affiliations),
                        (SELECT count(*) FROM center.completions WHERE action='system_correct')
                 FROM center.affiliations affiliation
                 JOIN center.profiles profile ON profile.id=affiliation.person_id
                 JOIN center.completions completion ON completion.operation_id=affiliation.operation_id
                 JOIN identity.persons person ON person.id=affiliation.person_id
                 WHERE affiliation.operation_id::text=$1",
                &[&operation],
            )
            .await
            .expect("one paired committed affiliation and permanent completion");
        assert!(facts.get::<_, bool>(0));
        assert!(facts.get::<_, bool>(1));
        assert_eq!(facts.get::<_, String>(2), "system");
        assert_eq!(facts.get::<_, String>(3), "system_correct");
        assert!(facts.get::<_, bool>(4));
        assert!(facts.get::<_, bool>(5));
        assert!(facts.get::<_, bool>(6));
        assert_eq!(facts.get::<_, i64>(7), 2);
        assert_eq!(facts.get::<_, i64>(8), 1);

        for dry_run in [false, true] {
            database
                .run(database.admin("correct-affiliation", &path, &operation, dry_run))
                .await
                .succeeds(Some("result=completed"));
            assert_eq!(snapshot(observer.client()).await, committed, "original replay is read-only");
        }
        let mut changed = input;
        changed["affiliation"] = json!("02");
        let changed = database.private("changed-affiliation.json", &changed);
        database
            .run(database.admin("correct-affiliation", &changed, &operation, false))
            .await
            .fails();
        assert_eq!(snapshot(observer.client()).await, committed, "mismatch writes no facts");

        // A committed result survives a failed stdout write; the same operation then replays.
        let current: String = observer
            .client()
            .query_one(
                "SELECT affiliation_id::text FROM center.profiles WHERE id::text=$1",
                &[&person],
            )
            .await
            .expect("current affiliation")
            .get(0);
        let second = database.private(
            "affiliation-second.json",
            &json!({
                "operator": {"mobile": MOBILE, "accountId": account, "revisionId": revision},
                "target": {
                    "accountId": account,
                    "personId": person,
                    "revisionId": revision,
                    "affiliationId": current,
                },
                "affiliation": "02",
            }),
        );
        let failed_operation = Uuid::new_v4().to_string();
        let full = fs::File::create("/dev/full").expect("full output device");
        let stderr = database.directory.path().join("full.stderr");
        let mut command = database.admin("correct-affiliation", &second, &failed_operation, false);
        command
            .stdout(Stdio::from(full))
            .stderr(fs::File::create(&stderr).expect("capture failed stderr"));
        let mut child = command.spawn().expect("actual deployment binary");
        let status = timeout(Duration::from_secs(30), child.wait())
            .await
            .expect("failed output finishes")
            .expect("failed output exit");
        assert!(!status.success(), "a failed stdout fails the command");
        assert!(!fs::read_to_string(&stderr)
            .expect("failed stderr")
            .contains(MOBILE));
        database
            .run(database.admin("correct-affiliation", &second, &failed_operation, false))
            .await
            .succeeds(Some("result=completed"));
        let after = snapshot(observer.client()).await;
        assert_eq!(
            observer
                .client()
                .query_one(
                    "SELECT (SELECT count(*) FROM center.affiliations),
                            (SELECT count(*) FROM center.completions WHERE action='system_correct')",
                    &[],
                )
                .await
                .expect("failed output retains its one committed result")
                .get::<_, i64>(0),
            3
        );
        database
            .run(database.admin("correct-affiliation", &second, &failed_operation, false))
            .await
            .succeeds(Some("result=completed"));
        assert_eq!(snapshot(observer.client()).await, after, "replay after failed output is read-only");
        observer.terminate().await.expect("end independent observer");
    });
}
