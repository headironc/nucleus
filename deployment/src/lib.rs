//! Offline deployment entrypoints.

pub mod access;
pub mod admin;
pub mod credentials;
pub mod database;
pub mod instance;
pub mod keys;
pub mod migration;
pub mod permissions;
pub mod settings;

use std::time::Duration;

pub const OPERATION_BUDGET: Duration = Duration::from_secs(300);

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::sync::{Arc, Mutex, Weak};

    use domain::keys::{Entry, Id, Material, metadata::Metadata};
    use platform::database::{
        Connections, assembly,
        credentials::Password,
        settings::{Budgets, Settings},
    };
    use tokio::runtime::Runtime;
    use uuid::Uuid;
    use zeroize::Zeroizing;

    pub(crate) struct Server {
        fixture: fixture::Server,
    }

    impl Server {
        pub(crate) fn new(fixture: fixture::Server) -> Self {
            Self { fixture }
        }

        pub(crate) fn connections(&self, name: &str, user: &str) -> Connections {
            self.connections_at(name, user, self.fixture.port())
        }

        pub(crate) fn fixture(&self) -> &fixture::Server {
            &self.fixture
        }

        pub(crate) fn connections_at(&self, name: &str, user: &str, port: u16) -> Connections {
            let roots = assembly::roots(
                &std::fs::read(self.fixture.authority()).expect("fixture certificate"),
            )
            .expect("fixture trust anchor");
            Connections::new(
                Settings::new(
                    "127.0.0.1".into(),
                    port,
                    name.into(),
                    user.into(),
                    2,
                    Budgets::default(),
                )
                .expect("settings"),
                Password::new(fixture::PASSWORD.into()).expect("password"),
                roots,
            )
            .expect("connections")
        }

        async fn database(&self) -> String {
            let name = format!("test_{}", Uuid::new_v4().simple());
            let admin = self
                .connections("postgres", "postgres")
                .recover()
                .await
                .expect("admin");
            // Only a generated UUID hex identifier is interpolated.
            admin
                .client()
                .batch_execute(&format!(r#"CREATE DATABASE {name}"#))
                .await
                .expect("database");
            admin.terminate().await.expect("end admin");
            name
        }
    }

    static SERVER: Mutex<Weak<Server>> = Mutex::new(Weak::new());

    pub(crate) fn server() -> Arc<Server> {
        let mut shared = SERVER.lock().expect("fixture lock");
        if let Some(server) = shared.upgrade() {
            return server;
        }
        let server = Arc::new(Server::new(fixture::Server::start()));
        Runtime::new().expect("runtime").block_on(async {
            let session = server
                .connections("postgres", "postgres")
                .recover()
                .await
                .expect("admin");
            for user in ["steward", "clerk"] {
                let sql: String = session
                    .client()
                    .query_one(
                        r#"SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', $1::text, $2::text)"#,
                        &[&user, &fixture::PASSWORD],
                    )
                    .await
                    .expect("quote role")
                    .get(0);
                session
                    .client()
                    .batch_execute(&sql)
                    .await
                    .expect("create fictional role");
            }
            session.terminate().await.expect("admin ended");
        });
        *shared = Arc::downgrade(&server);
        server
    }

    /// The fence a deployment command would hold, taken and released on its own connections.
    pub(crate) async fn fence(server: &Server, name: &str) -> domain::database::instance::Fence {
        fence_as(server, name, "steward").await
    }

    pub(crate) async fn fence_as(
        server: &Server,
        name: &str,
        user: &str,
    ) -> domain::database::instance::Fence {
        let lock = server.connections(name, user);
        // An earlier holder's backend may still be ending; wait for it like a rerun would.
        let held = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match crate::instance::take(&lock).await {
                    Ok(held) => break held,
                    Err(domain::runtime::Error::InstanceHeld) => {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("deployment instance lock failed: {error:?}"),
                }
            }
        })
        .await
        .expect("deployment instance lock");
        let fence = held.fence();
        held.release().await.expect("release the instance lock");
        lock.close().await;
        assert!(released(server, name).await, "the helper released its lock");
        fence
    }

    /// The server ends a closed session's backend asynchronously; wait up to ten seconds
    /// for the instance lock to be released.
    pub(crate) async fn released(server: &Server, name: &str) -> bool {
        let observer = server
            .connections(name, "postgres")
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

    pub(crate) async fn database(server: &Server) -> String {
        database_for(server, "steward", "clerk").await
    }

    #[test]
    fn reuses_the_prepared_server() {
        let first = server();
        let second = server();
        assert!(Arc::ptr_eq(&first, &second));
        Runtime::new().expect("runtime").block_on(async {
            for user in ["steward", "clerk"] {
                let session = second
                    .connections("postgres", user)
                    .recover()
                    .await
                    .expect("prepared login");
                session.terminate().await.expect("end login");
            }
        });
    }

    pub(crate) async fn database_for(
        server: &Server,
        deployment_user: &str,
        runtime_user: &str,
    ) -> String {
        let name = server.database().await;
        let admin = server
            .connections(&name, "postgres")
            .recover()
            .await
            .expect("database admin");
        admin.client().query_one(
            r#"SELECT set_config('nucleus.deployment_user', $1, false), set_config('nucleus.runtime_user', $2, false)"#,
            &[&deployment_user, &runtime_user],
        ).await.expect("bind fictional roles");
        admin
            .client()
            .batch_execute(include_str!("../roles.sql"))
            .await
            .expect("validate roles");
        admin
            .client()
            .batch_execute(include_str!("../prepare.sql"))
            .await
            .expect("prepare database");
        admin.terminate().await.expect("end preparation");
        name
    }

    /// Two fictional root versions; `changed` replaces version 0's material with another.
    pub(crate) fn metadata(active_id: i64, changed: bool) -> Metadata {
        let materials = [
            if changed {
                "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio"
            } else {
                "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"
            },
            "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE",
        ];
        let entries = [0, 1]
            .into_iter()
            .zip(materials)
            .map(|(id, material)| {
                Entry::new(
                    Id::new(id).expect("id"),
                    Material::new(Zeroizing::new(material.into())).expect("material"),
                )
            })
            .collect();
        domain::keys::Keys::new(Id::new(active_id).expect("id"), entries)
            .expect("root versions")
            .metadata()
            .clone()
    }

    #[test]
    fn deployment_failures_retain_the_query_and_terminated_driver() {
        use std::{error::Error, time::Duration};

        use fixture::database_proxy::{Fault, Proxy};
        use platform::database::{Session, User};
        use tokio::time::timeout;

        async fn snapshot(observer: &Session) -> Vec<String> {
            let history: String = observer.client().query_one(
                "SELECT jsonb_agg(jsonb_build_array(version, name, applied_on, checksum, xmin::text) ORDER BY version)::text FROM deployment.migrations",
                &[],
            ).await.expect("independent migration history").get(0);
            let mut facts = vec![history];
            facts.push(
                observer
                    .client()
                    .query_one(
                        "SELECT jsonb_agg(jsonb_build_array(id, encode(fingerprint, 'hex'), active, xmin::text) ORDER BY id)::text FROM security.keys",
                        &[],
                    )
                    .await
                    .expect("independent key registration")
                    .get(0),
            );
            facts
        }

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let name = database(&server).await;
            let connections = server.connections(&name, "steward");
            let user = User::new("clerk".into()).expect("runtime identity");
            crate::migration::run(&connections, &user).await.expect("migrations");
            crate::keys::initialize(&connections, &metadata(0, false), crate::tests::fence(&server, &name).await).await.expect("keys");
            let observer = server.connections(&name, "postgres").recover().await.expect("independent observer");
            let before = snapshot(&observer).await;

            for operation in ["migration", "keys", "permissions"] {
                let proxy = Proxy::start(&server.fixture).await;
                proxy.arm(Fault::StalledQuery);
                let connections = server.connections_at(&name, "steward", proxy.port());
                let user = user.clone();
                let fence = crate::tests::fence(&server, &name).await;
                let mut task = tokio::spawn(async move {
                    match operation {
                        "migration" => crate::migration::run(&connections, &user).await?,
                        "keys" => crate::keys::initialize(&connections, &metadata(0, false), fence).await?,
                        _ => crate::permissions::verify(&connections, &user).await?,
                    }
                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                });
                if timeout(Duration::from_secs(5), async {
                    tokio::select! {
                        _ = proxy.intercepted() => {},
                        result = &mut task => panic!("{operation}: completed before its prerequisite query: {result:?}"),
                    }
                }).await.is_err() {
                    panic!("{operation}: prerequisite query was not intercepted");
                }
                assert!(!task.is_finished(), "{operation}: query awaits the held Parse frame");
                timeout(Duration::from_secs(5), proxy.stop()).await.expect("disconnect the fixture transport");
                let error = timeout(Duration::from_secs(5), task).await.expect("failure budget").expect("deployment task").expect_err("query and driver both fail");
                let diagnostic = format!("{error:?}");
                let (query, termination) = match operation {
                    "migration" => {
                        let Some(crate::migration::Error::Combined { cause, termination }) = error.downcast_ref() else {
                            panic!("{operation}: missing combined failure: {diagnostic}");
                        };
                        let crate::migration::Error::Database(query) = cause.as_ref() else {
                            panic!("{operation}: missing original query failure: {diagnostic}");
                        };
                        (query, termination)
                    }
                    "keys" => {
                        let Some(crate::keys::Error::Combined { cause, termination }) = error.downcast_ref() else {
                            panic!("{operation}: missing combined failure: {diagnostic}");
                        };
                        let crate::keys::Error::Database(query) = cause.as_ref() else {
                            panic!("{operation}: missing original query failure: {diagnostic}");
                        };
                        (query, termination)
                    }
                    _ => {
                        let Some(crate::permissions::Error::Combined { cause, termination }) = error.downcast_ref() else {
                            panic!("{operation}: missing combined failure: {diagnostic}");
                        };
                        let crate::permissions::Error::Database(domain::database::Error::Query(query)) = cause.as_ref() else {
                            panic!("{operation}: missing original query failure: {diagnostic}");
                        };
                        let query = query.source().and_then(|source| source.downcast_ref::<tokio_postgres::Error>()).expect("retained original driver query error");
                        (query, termination)
                    }
                };
                assert!(query.is_closed(), "{operation}: interrupted query retains the closed-connection cause");
                let platform::database::Error::Postgres(driver) = termination else {
                    panic!("{operation}: missing actual driver error: {diagnostic}");
                };
                assert!(driver.source().and_then(|source| source.downcast_ref::<std::io::Error>()).is_some(), "{operation}: driver retains its actual transport error");
                assert!(!diagnostic.contains(&name));
                assert!(!diagnostic.contains(fixture::PASSWORD));
                assert!(snapshot(&observer).await == before, "{operation}: original persistent facts remain unchanged before any rerun");
            }
            crate::migration::run(&connections, &user).await.expect("migration history unchanged");
            crate::keys::initialize(&connections, &metadata(0, false), crate::tests::fence(&server, &name).await).await.expect("metadata unchanged");
            crate::permissions::verify(&connections, &user).await.expect("ownership and runtime permissions unchanged");
            observer.terminate().await.expect("end observer");
        });
    }
}
