//! Offline deployment entrypoints.

pub mod credentials;
pub mod database;
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

    use domain::identity::keys::{self, challenge, encryption, metadata::Metadata, mobile};
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

        fn connections_at(&self, name: &str, user: &str, port: u16) -> Connections {
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

    pub(crate) fn metadata(active_id: i64, changed: bool) -> Metadata {
        fn entries(changed: bool) -> Vec<keys::Entry> {
            [
                (
                    0,
                    if changed {
                        "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio"
                    } else {
                        "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"
                    },
                ),
                (1, "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE"),
            ]
            .into_iter()
            .map(|(id, material)| {
                keys::Entry::new(
                    keys::Id::new(id).expect("id"),
                    keys::Material::new(Zeroizing::new(material.into())).expect("material"),
                )
            })
            .collect()
        }
        let active_id = keys::Id::new(active_id).expect("id");
        Metadata::new(
            &challenge::Keys::new(active_id, entries(changed)).expect("challenge"),
            &mobile::Keys::new(active_id, entries(false)).expect("mobile"),
            &encryption::Keys::new(active_id, entries(false)).expect("encryption"),
            &keys::message::Keys::new(active_id, entries(false)).expect("message"),
        )
    }

    #[test]
    fn deployment_failures_retain_the_query_and_terminated_driver() {
        use std::time::Duration;

        use platform::database::User;
        use tokio::time::timeout;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let name = database(&server).await;
            let connections = server.connections(&name, "steward");
            let user = User::new("clerk".into()).expect("runtime identity");
            crate::migration::run(&connections, &user).await.expect("migrations");
            crate::keys::initialize(&connections, &metadata(0, false)).await.expect("keys");

            for operation in 0..3 {
                let proxy = fixture::database_proxy::Proxy::start(&server.fixture).await;
                let blocker = server.connections(&name, "postgres").recover().await.expect("fixture blocker");
                // Only this disposable database's catalogs are locked. Each entrypoint is
                // stopped during prerequisite queries, before any business write can start.
                blocker.client().batch_execute("BEGIN; LOCK TABLE pg_catalog.pg_namespace IN ACCESS EXCLUSIVE MODE").await.expect("hold prerequisite queries");
                let connections = server.connections_at(&name, "steward", proxy.port());
                let user = user.clone();
                let task = tokio::spawn(async move {
                    match operation {
                        0 => crate::migration::run(&connections, &user).await?,
                        1 => crate::keys::initialize(&connections, &metadata(0, false)).await?,
                        _ => crate::permissions::verify(&connections, &user).await?,
                    }
                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                });
                timeout(Duration::from_secs(5), async {
                    loop {
                        blocker.client().batch_execute("SELECT pg_catalog.pg_stat_clear_snapshot()").await.expect("refresh fixture activity snapshot");
                        let option = blocker.client().query_opt(
                            "SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname = $1 AND usename = 'steward' AND state = 'active' AND wait_event_type = 'Lock'",
                            &[&name],
                        ).await.expect("observe the blocked deployment query");
                        if option.is_some() {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("deployment reaches the protected catalog");
                timeout(Duration::from_secs(5), proxy.stop()).await.expect("disconnect the fixture transport");
                let error = timeout(Duration::from_secs(5), task).await.expect("failure budget").expect("deployment task").expect_err("query and driver both fail");
                let diagnostic = format!("{error:?}");
                assert!(diagnostic.starts_with("Combined {"), "{diagnostic}");
                assert!(diagnostic.contains("termination: database driver connection failed"), "{diagnostic}");
                assert!(!diagnostic.contains(&name));
                assert!(!diagnostic.contains(fixture::PASSWORD));
                blocker.client().batch_execute("ROLLBACK").await.expect("release fixture locks");
                blocker.terminate().await.expect("end blocker");
            }
            crate::migration::run(&connections, &user).await.expect("migration history unchanged");
            crate::keys::initialize(&connections, &metadata(0, false)).await.expect("metadata unchanged");
            crate::permissions::verify(&connections, &user).await.expect("ownership and runtime permissions unchanged");
        });
    }
}
