//! Isolated PostgreSQL databases for the served application.
//!
//! Each database runs the real migrations and Access initialization. Direct SQL elsewhere is
//! limited to setup, fault injection and observation that have no public HTTP equivalent.

use std::sync::{Arc, Mutex, Weak};

use platform::database::{self, Connections, credentials::Password};
use rustls::RootCertStore;
use tokio::runtime::Runtime;
use uuid::Uuid;

use crate::application::keys;

static SERVER: Mutex<Weak<fixture::Server>> = Mutex::new(Weak::new());

/// Files share the already-running fixture server's owned temporary-directory lifetime.
pub(crate) fn avatar_storage() -> domain::identity::avatar::StorageSettings {
    let server = SERVER
        .lock()
        .expect("fixture lock")
        .upgrade()
        .expect("active fixture server");
    let (root, mount, device) = server.files();
    domain::identity::avatar::StorageSettings::new(root, mount, device)
        .expect("real avatar storage settings")
}

pub(crate) fn server() -> Arc<fixture::Server> {
    let mut shared = SERVER.lock().expect("fixture lock");
    if let Some(server) = shared.upgrade() {
        return server;
    }
    let server = Arc::new(fixture::Server::start());
    Runtime::new().expect("runtime").block_on(async {
        let session = connections(&server, "postgres", "postgres")
            .recover()
            .await
            .expect("admin");
        for user in ["steward", "clerk"] {
            let sql: String = session
                .client()
                .query_one(
                    "SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', $1::text, $2::text)",
                    &[&user, &fixture::PASSWORD],
                )
                .await
                .expect("quote role")
                .get(0);
            session
                .client()
                .batch_execute(&sql)
                .await
                .expect("fictional role");
        }
        session.terminate().await.expect("end admin");
    });
    *shared = Arc::downgrade(&server);
    server
}

pub(crate) fn connections(server: &fixture::Server, name: &str, user: &str) -> Connections {
    let mut roots = RootCertStore::empty();
    roots
        .add(server.certificate().clone())
        .expect("fixture trust anchor");
    Connections::new(
        database::Settings::new(
            "127.0.0.1".into(),
            server.port(),
            name.into(),
            user.into(),
            2,
            database::settings::Budgets::default(),
        )
        .expect("connection settings"),
        Password::new(fixture::PASSWORD.into()).expect("fictional password"),
        roots,
    )
    .expect("connections")
}

pub(crate) async fn empty_database(server: &fixture::Server) -> String {
    let name = format!("test_{}", Uuid::new_v4().simple());
    let session = connections(server, "postgres", "postgres")
        .recover()
        .await
        .expect("admin");
    session
        .client()
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .await
        .expect("isolated database");
    session.terminate().await.expect("end admin");
    let session = connections(server, &name, "postgres")
        .recover()
        .await
        .expect("prepare");
    session
        .client()
        .batch_execute(
            "SET nucleus.deployment_user = 'steward'; SET nucleus.runtime_user = 'clerk'",
        )
        .await
        .expect("bind roles");
    session
        .client()
        .batch_execute(include_str!("../../deployment/roles.sql"))
        .await
        .expect("roles");
    session
        .client()
        .batch_execute(include_str!("../../deployment/prepare.sql"))
        .await
        .expect("prepare");
    session.terminate().await.expect("end preparation");
    let mut session = connections(server, &name, "steward")
        .recover()
        .await
        .expect("deployment");
    session.client().batch_execute("SET nucleus.runtime_user = 'clerk'; CREATE SCHEMA deployment; REVOKE ALL ON SCHEMA deployment FROM PUBLIC").await.expect("migration setup");
    let transaction = session
        .client_mut()
        .transaction()
        .await
        .expect("migration transaction");
    for (name, sql) in deployment::migration::MIGRATIONS {
        transaction
            .batch_execute(sql)
            .await
            .unwrap_or_else(|error| panic!("migration {name}: {error}"));
    }
    transaction
        .batch_execute(&domain::database::permissions::grants())
        .await
        .expect("runtime grants");
    assert!(matches!(
        domain::keys::database::initialize(keys(0).metadata(), &transaction)
            .await
            .expect("key initialization"),
        domain::database::transaction::Outcome::Pending(())
    ));
    transaction.commit().await.expect("commit migrations");
    session.terminate().await.expect("end deployment");
    name
}

pub(crate) async fn initialize(
    server: &fixture::Server,
    name: &str,
    mobile: &str,
) -> domain::access::initialize::Receipt {
    let keys = domain::access::Keys::new(keys(0));
    let operator = serde_json::from_value(serde_json::json!({"mobile":mobile,"name":"Fixture Operator","idNumber":"000000200002290010"})).expect("fictitious initializer");
    let context = domain::runtime::Context::new(connections(server, name, "postgres"))
        .await
        .expect("initialization clock");
    let mut owner = context
        .connections()
        .recover()
        .await
        .expect("initialization owner");
    let transaction = owner
        .client_mut()
        .transaction()
        .await
        .expect("atomic initialization");
    // Retained R2 fixtures move back at most 74 hours; keep one hour before their first appointment.
    let at = transaction
        .query_one("SELECT clock_timestamp() - interval '75 hours'", &[])
        .await
        .expect("historical initialization time")
        .get(0);
    let receipt = domain::access::initialize::seed(&transaction, &keys, &operator, at)
        .await
        .expect("complete identity and access");
    transaction
        .commit()
        .await
        .expect("initialization committed");
    owner
        .terminate()
        .await
        .expect("initialization driver ended");
    context.stop().await.expect("initialization clock stopped");
    receipt
}

pub(crate) async fn database(server: &fixture::Server) -> String {
    let name = empty_database(server).await;
    initialize(server, &name, "13900000000").await;
    name
}

pub(crate) fn proxy_connections(name: &str, proxy: &fixture::database_proxy::Proxy) -> Connections {
    let mut roots = RootCertStore::empty();
    roots.add(proxy.authority()).expect("proxy trust anchor");
    Connections::new(
        database::Settings::new(
            "127.0.0.1".into(),
            proxy.port(),
            name.into(),
            "clerk".into(),
            2,
            database::settings::Budgets::default(),
        )
        .expect("proxy settings"),
        Password::new(fixture::PASSWORD.into()).expect("password"),
        roots,
    )
    .expect("proxy connections")
}

pub(crate) async fn age_issuance(
    server: &fixture::Server,
    name: &str,
    registration: &domain::registration::Service,
) {
    let mut owner = connections(server, name, "postgres")
        .recover()
        .await
        .expect("isolated history owner");
    registration
        .age_for_test(owner.client_mut(), chrono::TimeDelta::seconds(61))
        .await
        .expect("consistent issuance history and holder seals");
    owner.terminate().await.expect("owner ended");
}

/// Probe the database instance lock from an unrelated session without waiting for it.
/// "Held" is immediate; a probe that took the lock waits until the server released it,
/// so the probe itself never blocks a later start.
pub(crate) async fn instance_held(server: &fixture::Server, name: &str) -> bool {
    let probe = connections(server, name, "clerk");
    let held = match domain::database::instance::Instance::hold(&probe).await {
        Ok(instance) => {
            instance.release().await.expect("release the probe lock");
            assert!(
                instance_released(server, name).await,
                "the probe released its lock"
            );
            false
        }
        Err(domain::runtime::Error::InstanceHeld) => true,
        Err(error) => panic!("instance lock probe failed: {error:?}"),
    };
    probe.close().await;
    held
}

/// The server ends a closed session's backend asynchronously; wait up to ten seconds
/// for the instance lock to be released.
pub(crate) async fn instance_released(server: &fixture::Server, name: &str) -> bool {
    let observer = connections(server, name, "postgres")
        .recover()
        .await
        .expect("instance lock observer");
    let released = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while observer
            .client()
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND classid = 1314210636 AND objid = 3 AND objsubid = 2 AND granted AND database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database()))",
                &[],
            )
            .await
            .expect("instance lock observation")
            .get::<_, bool>(0)
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    observer.terminate().await.expect("observer ended");
    released
}

/// Replay the original Access initialization; returns whether its receipt changed.
pub(crate) async fn replay_initialization(
    server: &fixture::Server,
    name: &str,
    mobile: &str,
    original: &domain::access::initialize::Receipt,
) -> bool {
    let context = domain::runtime::Context::hold(connections(server, name, "clerk"))
        .await
        .expect("maintenance context");
    let barrier = domain::database::Barrier::begin(context.clone())
        .await
        .expect("maintenance barrier");
    let administration = domain::startup::Administration::start(
        barrier,
        keys(0),
        "website-fixture".parse().expect("configured website app"),
    )
    .await
    .expect("verified maintenance startup");
    administration.confirm().await.expect("maintenance ready");
    let operator = serde_json::from_value(serde_json::json!({"mobile":mobile,"name":"Fixture Operator","idNumber":"000000200002290010"})).expect("original input");
    let receipt = administration
        .initialize(original.operation_id(), operator, false)
        .await
        .expect("online UUID does not disturb initialization replay")
        .expect("initialization receipt");
    administration.close();
    administration.drain().await.expect("maintenance drained");
    context.stop().await.expect("context ended");
    receipt != *original
}
