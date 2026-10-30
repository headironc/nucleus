//! TLS database connections and owned drivers. Transaction outcomes remain with the caller.

pub mod assembly;
pub mod credentials;
mod lease;
pub mod settings;
pub mod user;

use std::{
    fmt,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use deadpool::{Runtime, managed};
use rustls::{ClientConfig, RootCertStore, crypto::ring};
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::{AbortHandle, JoinError, JoinHandle},
    time::timeout,
};
use tokio_postgres::{Client, Config, config::SslMode};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::database::credentials::Password;
pub use crate::database::{lease::Lease, settings::Settings, user::User};

/// Online connections share a pool; recovery has one separate connection allowance.
pub struct Connections {
    pool: managed::Pool<Manager>,
    recovery: Arc<Semaphore>,
}

impl Connections {
    /// Assemble without reading files, environment variables, or connecting.
    ///
    /// Only the supplied certificate authorities are trusted. The root loader owns
    /// certificate and password sources. The caller selects the login identity.
    pub fn new(
        settings: Settings,
        password: Password,
        roots: RootCertStore,
    ) -> Result<Self, BuildError> {
        if roots.is_empty() {
            return Err(BuildError::EmptyAuthority);
        }
        let tls = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let mut config = Config::new();
        config
            .host(settings.host())
            .port(settings.port())
            .dbname(settings.name())
            .user(settings.user().as_ref())
            .password(password.as_ref())
            .ssl_mode(SslMode::Require)
            .connect_timeout(settings.budgets().connect())
            .options("-c search_path=");
        let capacity = settings.capacity();
        let budgets = settings.budgets();
        let pool = managed::Pool::builder(Manager(Connector {
            config,
            tls: MakeRustlsConnect::new(tls),
            settings,
            drivers: Mutex::new(Vec::new()),
        }))
        .max_size(capacity)
        .wait_timeout(Some(budgets.wait()))
        .create_timeout(Some(budgets.connect()))
        .recycle_timeout(Some(budgets.recycle()))
        .runtime(Runtime::Tokio1)
        .build()?;
        Ok(Self {
            pool,
            recovery: Arc::new(Semaphore::new(1)),
        })
    }

    /// Borrow an online session within the configured wait and connection budgets.
    pub async fn acquire(&self) -> Result<managed::Object<Manager>, Error> {
        Ok(self.pool.get().await?)
    }

    /// A write owner must explicitly confirm reuse; cancellation removes the session.
    pub async fn lease(&self) -> Result<Lease, Error> {
        Ok(Lease::new(self.pool.get().await?))
    }

    /// The consumer bounds dedicated capacity and owns shutdown (trusted time uses one).
    pub async fn dedicated(&self) -> Result<Session, Error> {
        if self.pool.is_closed() {
            return Err(managed::PoolError::Closed.into());
        }
        Ok(self.pool.manager().0.connect().await?)
    }

    /// A dedicated session for a process-lifetime lock, outside the online pool.
    ///
    /// TCP keepalive probes the peer every five seconds while idle, so a lost database
    /// host ends the driver instead of leaving the lock owner waiting for a reply.
    /// The returned [`Ended`] resolves once the driver stops for any reason.
    pub async fn instance(&self) -> Result<(Session, Ended), Error> {
        if self.pool.is_closed() {
            return Err(managed::PoolError::Closed.into());
        }
        Ok(self.pool.manager().0.instance().await?)
    }

    /// After consumers drain, close admission and await even drivers aborted by Drop.
    pub async fn close(&self) {
        self.pool.close();
        self.recovery.close();
        let connector = &self.pool.manager().0;
        let drivers = connector.drivers().clone();
        for driver in &drivers {
            driver.abort();
        }
        for driver in drivers {
            while !driver.is_finished() {
                tokio::task::yield_now().await;
            }
        }
        connector.drivers().retain(|driver| !driver.is_finished());
    }

    /// Open a fresh session without consuming any online pool slot.
    ///
    /// The allowance belongs to the driver, so cancellation cannot release it
    /// while that driver is still running. This does not acquire a database barrier.
    pub async fn recover(&self) -> Result<Session, Error> {
        let connector = &self.pool.manager().0;
        let permit = timeout(
            connector.settings.budgets().wait(),
            Arc::clone(&self.recovery).acquire_owned(),
        )
        .await
        .map_err(Error::RecoveryWait)?
        .map_err(Error::RecoveryClosed)?;
        Ok(connector.recover(permit).await?)
    }

    /// Remove a session permanently and wait until its local driver has ended.
    ///
    /// This is not proof of rollback. The transaction owner must subsequently
    /// cross the recovery barrier and inspect persistent execution evidence.
    pub async fn discard(session: managed::Object<Manager>) -> Result<(), Error> {
        managed::Object::take(session).terminate().await
    }
}

struct Connector {
    config: Config,
    tls: MakeRustlsConnect,
    settings: Settings,
    drivers: Mutex<Vec<AbortHandle>>,
}

impl Connector {
    fn drivers(&self) -> std::sync::MutexGuard<'_, Vec<AbortHandle>> {
        match self.drivers.lock() {
            Ok(drivers) => drivers,
            // This registry holds only task handles, never business decisions.
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn spawn(
        &self,
        future: impl Future<Output = Result<(), tokio_postgres::Error>> + Send + 'static,
    ) -> JoinHandle<Result<(), tokio_postgres::Error>> {
        let driver = tokio::spawn(future);
        let mut drivers = self.drivers();
        drivers.retain(|driver| !driver.is_finished());
        drivers.push(driver.abort_handle());
        driver
    }

    async fn connect(&self) -> Result<Session, ConnectionError> {
        let (client, connection) = self.open(&self.config).await?;
        let driver = self.spawn(connection);
        Ok(Session {
            client,
            driver,
            stopped: false,
        })
    }

    /// Unacknowledged probes or writes end the connection after about fifteen seconds.
    fn instance_config(&self) -> Config {
        let mut config = self.config.clone();
        config
            .keepalives(true)
            .keepalives_idle(KEEPALIVE)
            .keepalives_interval(KEEPALIVE)
            .keepalives_retries(3)
            .tcp_user_timeout(KEEPALIVE * 3);
        config
    }

    async fn instance(&self) -> Result<(Session, Ended), ConnectionError> {
        let (client, connection) = self.open(&self.instance_config()).await?;
        // The sender lives in the driver future, so completion and abortion both drop it.
        let (sender, receiver) = watch::channel(());
        let driver = self.spawn(async move {
            let _ended = sender;
            connection.await
        });
        Ok((
            Session {
                client,
                driver,
                stopped: false,
            },
            Ended(receiver),
        ))
    }

    async fn recover(&self, permit: OwnedSemaphorePermit) -> Result<Session, ConnectionError> {
        let (client, connection) = self.open(&self.config).await?;
        let driver = self.spawn(async move {
            let _permit = permit;
            connection.await
        });
        Ok(Session {
            client,
            driver,
            stopped: false,
        })
    }

    async fn open(
        &self,
        config: &Config,
    ) -> Result<
        (
            Client,
            impl Future<Output = Result<(), tokio_postgres::Error>> + Send + 'static,
        ),
        ConnectionError,
    > {
        timeout(
            self.settings.budgets().connect(),
            config.connect(self.tls.clone()),
        )
        .await?
        .map_err(ConnectionError::Connect)
    }
}

const KEEPALIVE: Duration = Duration::from_secs(5);

/// Observes the end of an instance session's driver without owning the session.
pub struct Ended(watch::Receiver<()>);

impl Ended {
    /// Resolve once the driver has stopped, including after a stop or abort.
    pub async fn wait(&mut self) {
        while self.0.changed().await.is_ok() {}
    }
}

/// Pool implementation detail; it never owns business SQL or transaction outcomes.
pub struct Manager(Connector);

impl managed::Manager for Manager {
    type Type = Session;
    type Error = ConnectionError;

    async fn create(&self) -> Result<Session, ConnectionError> {
        self.0.connect().await
    }

    async fn recycle(
        &self,
        session: &mut Session,
        _: &managed::Metrics,
    ) -> managed::RecycleResult<ConnectionError> {
        if session.client.is_closed() || session.driver.is_finished() {
            return Err(managed::RecycleError::message("database driver ended"));
        }
        // Returning a session is permitted only after a known transaction outcome.
        // Reset session-local state before lending it to an unrelated caller.
        for statement in ["ROLLBACK", "DISCARD ALL", "SET search_path TO ''"] {
            session
                .client
                .batch_execute(statement)
                .await
                .map_err(ConnectionError::Recycle)?;
        }
        Ok(())
    }
}

/// A client and the task that owns its socket have one lifetime.
pub struct Session {
    client: Client,
    driver: JoinHandle<Result<(), tokio_postgres::Error>>,
    stopped: bool,
}

impl Session {
    /// Access for queries. Do not return uncertain sessions to the pool.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Access for a short transaction, supervised by the owning use case.
    pub fn client_mut(&mut self) -> &mut Client {
        &mut self.client
    }

    /// Request driver cancellation; only stop or terminate waits for it to finish.
    pub fn abort(&self) {
        self.driver.abort();
    }

    /// Stop the local driver and await its termination, retaining unexpected errors.
    pub async fn terminate(mut self) -> Result<(), Error> {
        self.stop().await
    }

    /// Retain ownership across cancellation; repeated stops never poll a consumed result.
    /// Unexpected driver errors are reported by the call that completes the wait.
    pub async fn stop(&mut self) -> Result<(), Error> {
        if self.stopped {
            return Ok(());
        }
        self.abort();
        let result = (&mut self.driver).await;
        self.stopped = true;
        let error = match result {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) => error.into(),
            Err(error) if error.is_cancelled() => return Ok(()),
            Err(error) => Error::Driver(error),
        };
        Err(error)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Dropping a JoinHandle alone would detach the driver.
        self.abort();
    }
}

/// Diagnostics omit targets, passwords, SQL, and server-provided details.
#[derive(Error)]
pub enum BuildError {
    #[error("database certificate authorities are empty")]
    EmptyAuthority,
    #[error("invalid database TLS configuration")]
    Tls(#[from] rustls::Error),
    #[error("cannot build database pool")]
    Build(#[from] managed::BuildError),
}

impl fmt::Debug for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// Backend failures retain their phase without exposing server diagnostics.
#[derive(Error)]
pub enum ConnectionError {
    #[error("database connect failed")]
    Connect(#[source] tokio_postgres::Error),
    #[error("database recycle failed")]
    Recycle(#[source] tokio_postgres::Error),
    #[error("database connect budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
}

impl fmt::Debug for ConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// Runtime failures preserve causes and safe operation context.
#[derive(Error)]
pub enum Error {
    #[error("cannot acquire database session")]
    Pool(#[source] Box<managed::PoolError<ConnectionError>>),
    #[error("{0}")]
    Connection(#[from] ConnectionError),
    #[error("database driver connection failed")]
    Postgres(#[from] tokio_postgres::Error),
    #[error("database recovery wait budget expired")]
    RecoveryWait(#[source] tokio::time::error::Elapsed),
    #[error("database driver task failed")]
    Driver(#[from] JoinError),
    #[error("database recovery allowance closed")]
    RecoveryClosed(#[source] tokio::sync::AcquireError),
}

impl From<managed::PoolError<ConnectionError>> for Error {
    fn from(error: managed::PoolError<ConnectionError>) -> Self {
        Self::Pool(Box::new(error))
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::Pool(error) = self {
            return match error.as_ref() {
                managed::PoolError::Timeout(stage) => {
                    write!(formatter, "database pool {stage:?} budget expired")
                }
                managed::PoolError::Backend(error) => write!(formatter, "database pool: {error}"),
                managed::PoolError::Closed => formatter.write_str("database pool closed"),
                managed::PoolError::NoRuntimeSpecified => {
                    formatter.write_str("database pool runtime missing")
                }
                managed::PoolError::PostCreateHook(_) => {
                    formatter.write_str("database pool post-create hook failed")
                }
            };
        }
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{
        sync::{Arc, Mutex, Weak},
        time::Duration,
    };

    use rcgen::generate_simple_self_signed;
    use rustls::{ClientConfig, RootCertStore, crypto::ring};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        runtime::Runtime,
        sync::oneshot,
        time::timeout,
    };
    use tokio_postgres::{Config, NoTls, config::SslMode};
    use tokio_postgres_rustls::MakeRustlsConnect;
    use uuid::Uuid;

    use crate::database::{
        BuildError, ConnectionError, Connections, Error, Session, assembly,
        credentials::Password,
        settings::{Budgets, Settings},
    };

    const PASSWORD: &str = "fictional-database-test-password";
    static SERVER: Mutex<Weak<Server>> = Mutex::new(Weak::new());

    struct Server {
        fixture: fixture::Server,
    }

    impl Server {
        fn acquire() -> Arc<Self> {
            let mut shared = SERVER.lock().expect("fixture initialization lock");
            if let Some(server) = shared.upgrade() {
                return server;
            }
            let server = Arc::new(Self {
                fixture: fixture::Server::start(),
            });
            Runtime::new().expect("fixture runtime").block_on(async {
                let session = server.admin().await;
                session
                    .client()
                    .batch_execute(
                        "CREATE ROLE fixture LOGIN PASSWORD 'fictional-database-test-password'",
                    )
                    .await
                    .expect("create test application login");
                session.terminate().await.expect("end setup driver");
            });
            *shared = Arc::downgrade(&server);
            server
        }

        async fn admin(&self) -> Session {
            let mut roots = RootCertStore::empty();
            roots
                .add(self.fixture.certificate().clone())
                .expect("test trust anchor");
            let tls = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("ring TLS")
                .with_root_certificates(roots)
                .with_no_client_auth();
            let mut config = Config::new();
            config
                .host("127.0.0.1")
                .port(self.fixture.port())
                .user("postgres")
                .password(fixture::PASSWORD)
                .dbname("postgres")
                .ssl_mode(SslMode::Require);
            let (client, connection) = config
                .connect(MakeRustlsConnect::new(tls))
                .await
                .expect("test admin TLS connection");
            Session {
                client,
                driver: tokio::spawn(connection),
                stopped: false,
            }
        }

        async fn database(&self) -> String {
            let name = format!("test_{}", Uuid::new_v4().simple());
            let session = self.admin().await;
            // The identifier consists only of this fixed prefix and generated UUID hex.
            session
                .client()
                .batch_execute(&format!("CREATE DATABASE {name}"))
                .await
                .expect("isolated test database");
            session
                .terminate()
                .await
                .expect("end database setup driver");
            name
        }

        fn connections(&self, name: &str) -> Connections {
            let mut roots = RootCertStore::empty();
            roots
                .add(self.fixture.certificate().clone())
                .expect("test trust anchor");
            Connections::new(
                self.settings(name),
                Password::new(PASSWORD.into()).expect("valid fixture password"),
                roots,
            )
            .expect("assemble test connections")
        }

        fn settings(&self, name: &str) -> Settings {
            Settings::new(
                "127.0.0.1".into(),
                self.fixture.port(),
                name.into(),
                "fixture".into(),
                2,
                Budgets::new(
                    Duration::from_millis(100),
                    Duration::from_secs(5),
                    Duration::from_secs(3),
                )
                .expect("test budgets"),
            )
            .expect("test settings")
        }
    }

    #[test]
    fn runs_independent_transactions_and_recovers_with_a_full_pool() {
        let server = Server::acquire();
        Runtime::new().expect("test runtime").block_on(async {
            let name = server.database().await;
            let connections = server.connections(&name);
            let mut first = connections.acquire().await.expect("first online session");
            let user: String = first
                .client()
                .query_one("SELECT current_user", &[])
                .await
                .expect("online login identity")
                .get(0);
            assert_eq!(user, "fixture");
            let mut second = connections.acquire().await.expect("second online session");
            let transaction = first
                .client_mut()
                .transaction()
                .await
                .expect("first transaction");
            transaction
                .batch_execute("SELECT pg_advisory_xact_lock(101)")
                .await
                .expect("first independent lock");
            let other = second
                .client_mut()
                .transaction()
                .await
                .expect("second transaction");
            timeout(
                Duration::from_secs(2),
                other.batch_execute("SELECT pg_advisory_xact_lock(102)"),
            )
            .await
            .expect("independent transaction is not serialized")
            .expect("second lock");
            let error = match connections.acquire().await {
                Ok(_) => panic!("full online pool must time out"),
                Err(error) => error,
            };
            assert!(matches!(error, Error::Pool(_)));
            assert!(format!("{error:?}").contains("Wait"));
            let recovery = connections
                .recover()
                .await
                .expect("independent recovery slot");
            let row = recovery
                .client()
                .query_one(
                    "SELECT current_user, ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
                    &[],
                )
                .await
                .expect("recovery identity and TLS");
            assert_eq!(row.get::<_, String>(0), "fixture");
            assert!(row.get::<_, bool>(1));
            assert!(matches!(
                connections.recover().await,
                Err(Error::RecoveryWait(_))
            ));
            recovery.terminate().await.expect("end recovery driver");
            connections
                .recover()
                .await
                .expect("recovery allowance reused")
                .terminate()
                .await
                .expect("end next recovery");
            other
                .commit()
                .await
                .expect("commit independent transaction");
            transaction
                .rollback()
                .await
                .expect("rollback first transaction");
            Connections::discard(first).await.expect("end first driver");
            Connections::discard(second)
                .await
                .expect("end second driver");
        });
    }

    #[test]
    fn resets_reused_sessions_and_removes_discarded_drivers() {
        let server = Server::acquire();
        Runtime::new().expect("test runtime").block_on(async {
            let name = server.database().await;
            let connections = server.connections(&name);
            let first = connections.acquire().await.expect("online session");
            let pid: i32 = first
                .client()
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .expect("backend identity")
                .get(0);
            first
                .client()
                .batch_execute("SET search_path TO public; SELECT pg_advisory_lock(777)")
                .await
                .expect("session-local state");
            drop(first);
            let reused = connections.acquire().await.expect("recycled session");
            let row = reused
                .client()
                .query_one(
                    "SELECT pg_backend_pid(), current_setting('search_path')",
                    &[],
                )
                .await
                .expect("recycled state");
            assert_eq!(row.get::<_, i32>(0), pid);
            assert_eq!(row.get::<_, String>(1), "\"\"");
            let recovery = connections.recover().await.expect("recovery session");
            assert!(
                recovery
                    .client()
                    .query_one("SELECT pg_try_advisory_lock(777)", &[])
                    .await
                    .expect("session lock released")
                    .get::<_, bool>(0)
            );
            Connections::discard(reused)
                .await
                .expect("discard awaits driver");
            let replacement = connections.acquire().await.expect("replacement session");
            let replacement_pid: i32 = replacement
                .client()
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .expect("replacement identity")
                .get(0);
            assert_ne!(replacement_pid, pid);
            Connections::discard(replacement)
                .await
                .expect("end replacement driver");
            recovery.terminate().await.expect("end recovery driver");
        });
    }

    #[test]
    fn rejects_untrusted_certificates_wrong_passwords_and_plaintext() {
        let server = Server::acquire();
        Runtime::new().expect("test runtime").block_on(async {
            let name = server.database().await;
            let other =
                generate_simple_self_signed(vec!["localhost".into()]).expect("untrusted test CA");
            let untrusted = Connections::new(
                server.settings(&name),
                Password::new(PASSWORD.into()).expect("valid fixture password"),
                assembly::roots(other.cert.pem().as_bytes()).expect("unrelated trust anchor"),
            )
            .expect("valid unrelated CA");
            assert!(untrusted.acquire().await.is_err());
            let wrong_host = Settings::new(
                "localhost".into(),
                server.fixture.port(),
                name.clone(),
                "fixture".into(),
                2,
                Budgets::default(),
            )
            .expect("valid host without a matching certificate name");
            let mut roots = RootCertStore::empty();
            roots
                .add(server.fixture.certificate().clone())
                .expect("test trust anchor");
            let wrong_host = Connections::new(
                wrong_host,
                Password::new(PASSWORD.into()).expect("valid fixture password"),
                roots.clone(),
            )
            .expect("assemble hostname mismatch");
            assert!(wrong_host.acquire().await.is_err());
            let wrong = Connections::new(
                server.settings(&name),
                Password::new("wrong-test-password".into()).expect("valid password shape"),
                roots,
            )
            .expect("valid password shape");
            let error = match wrong.acquire().await {
                Ok(_) => panic!("wrong password accepted"),
                Err(error) => error,
            };
            assert!(!format!("{error:?} {error}").contains("wrong-test-password"));
            let mut plaintext = Config::new();
            plaintext
                .host("127.0.0.1")
                .port(server.fixture.port())
                .user("fixture")
                .password(PASSWORD)
                .dbname(&name)
                .ssl_mode(SslMode::Disable);
            assert!(plaintext.connect(NoTls).await.is_err());
        });
    }

    #[test]
    fn rejects_empty_trust_roots_without_connecting() {
        let settings = || {
            Settings::new(
                "invalid.example".into(),
                5432,
                "test".into(),
                "fixture".into(),
                2,
                Budgets::default(),
            )
            .expect("valid settings")
        };
        assert!(matches!(
            Connections::new(
                settings(),
                Password::new(PASSWORD.into()).expect("valid fixture password"),
                RootCertStore::empty()
            ),
            Err(BuildError::EmptyAuthority)
        ));
    }

    #[test]
    fn does_not_fall_back_to_plaintext_or_wait_forever_for_tls() {
        for refuse in [true, false] {
            Runtime::new().expect("test runtime").block_on(async {
                let listener = TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("local protocol probe");
                let port = listener.local_addr().expect("probe address").port();
                let (finished, finish) = oneshot::channel();
                let probe = tokio::spawn(async move {
                    timeout(Duration::from_secs(5), async {
                        let (mut socket, _) =
                            listener.accept().await.expect("probe accepts client");
                        let mut request = [0; 8];
                        socket
                            .read_exact(&mut request)
                            .await
                            .expect("PostgreSQL SSLRequest");
                        assert_eq!(request, [0, 0, 0, 8, 4, 210, 22, 47]);
                        if refuse {
                            socket.write_all(b"N").await.expect("server refuses TLS");
                            let mut byte = [0];
                            assert_eq!(
                                socket
                                    .read(&mut byte)
                                    .await
                                    .expect("client closes without startup message"),
                                0
                            );
                        } else {
                            finish.await.expect("client connection budget ends");
                        }
                    })
                    .await
                    .expect("bounded protocol probe");
                });
                let certificate =
                    generate_simple_self_signed(vec!["127.0.0.1".into()]).expect("test CA");
                let settings = Settings::new(
                    "127.0.0.1".into(),
                    port,
                    "test".into(),
                    "fixture".into(),
                    2,
                    Budgets::new(
                        Duration::from_secs(1),
                        Duration::from_millis(100),
                        Duration::from_secs(1),
                    )
                    .expect("short connect budget"),
                )
                .expect("probe settings");
                let connections = Connections::new(
                    settings,
                    Password::new(PASSWORD.into()).expect("valid fixture password"),
                    assembly::roots(certificate.cert.pem().as_bytes()).expect("test trust anchor"),
                )
                .expect("probe connector");
                let error = match timeout(Duration::from_secs(2), connections.recover())
                    .await
                    .expect("bounded connection attempt")
                {
                    Ok(_) => panic!("probe must reject or time out"),
                    Err(error) => error,
                };
                if refuse {
                    assert!(matches!(
                        error,
                        Error::Connection(ConnectionError::Connect(_))
                    ));
                } else {
                    assert!(matches!(
                        error,
                        Error::Connection(ConnectionError::Timeout(_))
                    ));
                    assert!(format!("{error:?}").contains("connect budget expired"));
                }
                if !refuse {
                    finished.send(()).expect("release probe");
                }
                probe.await.expect("probe completes");
            });
        }
    }

    #[test]
    fn proxy_release_after_backend_exit_preserves_next_interception() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = Server::acquire();
        Runtime::new().expect("test runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let name = server.database().await;
                let proxy = Proxy::start(&server.fixture).await;
                let mut roots = RootCertStore::empty();
                roots.add(proxy.authority()).expect("proxy authority");
                let connections = Connections::new(
                    Settings::new(
                        "127.0.0.1".into(),
                        proxy.port(),
                        name,
                        "fixture".into(),
                        2,
                        Budgets::default(),
                    )
                    .expect("proxy settings"),
                    Password::new(PASSWORD.into()).expect("fixture password"),
                    roots,
                )
                .expect("proxy connections");
                let first = connections.recover().await.expect("first reader");
                proxy.arm(Fault::StalledTime);
                let task = tokio::spawn(async move {
                    let result = first
                        .client()
                        .query_one("SELECT clock_timestamp()", &[])
                        .await;
                    let termination = first.terminate().await;
                    assert!(
                        matches!(termination, Ok(()) | Err(Error::Postgres(_))),
                        "unexpected driver termination: {termination:?}"
                    );
                    result
                });
                let pid = timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("first query stopped");
                let admin = server.admin().await;
                assert!(
                    admin
                        .client()
                        .query_one("SELECT pg_terminate_backend($1, 2000)", &[&pid])
                        .await
                        .expect("terminate the intercepted backend")
                        .get::<_, bool>(0)
                );
                assert!(
                    timeout(Duration::from_secs(2), task)
                        .await
                        .expect("reader observes terminated backend")
                        .expect("first task")
                        .is_err()
                );
                proxy.release();
                // Repeated cleanup cannot reserve a release for the next fault,
                // even if the old relay has not yet observed the backend's EOF.
                proxy.release();
                let second = connections.recover().await.expect("second reader");
                proxy.arm(Fault::StalledTime);
                let mut task = tokio::spawn(async move {
                    let result = second
                        .client()
                        .query_one("SELECT clock_timestamp()", &[])
                        .await;
                    second.terminate().await.expect("second driver ended");
                    result
                });
                let next = timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("second query stopped");
                assert_ne!(pid, next);
                assert!(
                    timeout(Duration::from_millis(100), &mut task)
                        .await
                        .is_err(),
                    "release for an ended fault must not release the next query"
                );
                proxy.release();
                timeout(Duration::from_secs(2), task)
                    .await
                    .expect("second query released")
                    .expect("second task")
                    .expect("second result");
                connections.close().await;
                admin.terminate().await.expect("observer ended");
                proxy.stop().await;
            })
            .await
            .expect("proxy release lifetime");
        });
    }

    #[test]
    fn drop_ends_recovery_driver_and_dead_online_sessions_are_replaced() {
        let server = Server::acquire();
        Runtime::new().expect("test runtime").block_on(async {
            let name = server.database().await;
            let connections = server.connections(&name);
            let recovery = connections.recover().await.expect("recovery session");
            let task = recovery.driver.abort_handle();
            drop(recovery);
            timeout(Duration::from_secs(2), async {
                while !task.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("dropped recovery does not leave a detached driver");
            connections
                .recover()
                .await
                .expect("released recovery allowance")
                .terminate()
                .await
                .expect("end recovery");
            let online = connections.acquire().await.expect("online session");
            let pid: i32 = online
                .client()
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .expect("backend identity")
                .get(0);
            let admin = server.admin().await;
            assert!(
                admin
                    .client()
                    .query_one("SELECT pg_terminate_backend($1, 2000)", &[&pid])
                    .await
                    .expect("terminate only this test session")
                    .get::<_, bool>(0)
            );
            assert!(online.client().query_one("SELECT 1", &[]).await.is_err());
            drop(online);
            let replacement = connections
                .acquire()
                .await
                .expect("failed session is not recycled");
            let next: i32 = replacement
                .client()
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .expect("new backend")
                .get(0);
            assert_ne!(next, pid);
            Connections::discard(replacement)
                .await
                .expect("end replacement");
            admin.terminate().await.expect("end test admin");
        });
    }

    #[test]
    fn cancelled_stop_retains_the_driver_and_repeated_stops_are_safe() {
        use std::{future::Future, task::Poll};

        let server = Server::acquire();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("single-threaded test runtime")
            .block_on(async {
                let name = server.database().await;
                let connections = server.connections(&name);
                let mut session = connections.dedicated().await.expect("dedicated session");
                {
                    let mut stop = std::pin::pin!(session.stop());
                    // The driver cannot process its abort until this thread yields.
                    std::future::poll_fn(|context| {
                        assert!(stop.as_mut().poll(context).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                }
                timeout(Duration::from_secs(1), session.stop())
                    .await
                    .expect("stop budget")
                    .expect("resume cancelled stop");
                assert!(session.driver.is_finished());
                assert!(session.client().query_one("SELECT 1", &[]).await.is_err());
                session.stop().await.expect("repeat completed stop");
                session.terminate().await.expect("consume stopped session");
                connections.close().await;
            });
    }

    #[test]
    fn stop_reports_a_prior_driver_error_before_allowing_repeated_cleanup() {
        let server = Server::acquire();
        Runtime::new().expect("test runtime").block_on(async {
            let name = server.database().await;
            let connections = server.connections(&name);
            let mut session = connections.dedicated().await.expect("dedicated session");
            let pid: i32 = session
                .client()
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .expect("session backend")
                .get(0);
            let admin = server.admin().await;
            let ended: bool = admin
                .client()
                .query_one("SELECT pg_terminate_backend($1)", &[&pid])
                .await
                .expect("terminate fixture backend")
                .get(0);
            assert!(ended);
            timeout(Duration::from_secs(2), async {
                while !session.driver.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("driver observes backend failure");
            let error = session.stop().await.expect_err("retain backend failure");
            assert!(matches!(error, Error::Postgres(ref cause)
                if cause.code() == Some(&tokio_postgres::error::SqlState::ADMIN_SHUTDOWN)));
            session
                .stop()
                .await
                .expect("repeat cleanup after reported failure");
            admin.terminate().await.expect("end admin");
            connections.close().await;
        });
    }

    #[test]
    fn instance_sessions_report_their_driver_end_and_probe_the_peer() {
        let server = Server::acquire();
        Runtime::new().expect("test runtime").block_on(async {
            let name = server.database().await;
            let connections = server.connections(&name);
            let (mut session, mut ended) = connections.instance().await.expect("instance session");
            let pid: i32 = session
                .client()
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .expect("instance backend")
                .get(0);
            let mut waiting = std::pin::pin!(ended.wait());
            assert!(
                timeout(Duration::from_millis(100), &mut waiting)
                    .await
                    .is_err(),
                "a live driver has not ended"
            );
            let admin = server.admin().await;
            let terminated: bool = admin
                .client()
                .query_one("SELECT pg_terminate_backend($1)", &[&pid])
                .await
                .expect("terminate instance backend")
                .get(0);
            assert!(terminated);
            timeout(Duration::from_secs(2), waiting)
                .await
                .expect("backend termination ends the observed driver");
            // The end notice is sent from inside the driver task, which finishes just after it.
            timeout(Duration::from_secs(2), async {
                while !session.driver.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the driver task finishes after reporting its end");
            assert!(session.stop().await.is_err(), "retain the backend failure");

            let (session, mut ended) = connections.instance().await.expect("second instance");
            session.terminate().await.expect("local stop");
            timeout(Duration::from_secs(1), ended.wait())
                .await
                .expect("a local stop also ends the observation");
            let connector = &connections.pool.manager().0;
            let instance = connector.instance_config();
            assert!(instance.get_keepalives());
            assert_eq!(instance.get_keepalives_idle(), Duration::from_secs(5));
            assert_eq!(
                instance.get_keepalives_interval(),
                Some(Duration::from_secs(5))
            );
            assert_eq!(instance.get_keepalives_retries(), Some(3));
            assert_eq!(
                instance.get_tcp_user_timeout(),
                Some(&Duration::from_secs(15))
            );
            assert_eq!(
                connector.config.get_keepalives_idle(),
                Duration::from_secs(2 * 60 * 60),
                "online sessions keep the driver default"
            );
            connections.close().await;
            assert!(connections.instance().await.is_err(), "closed admission");
            admin.terminate().await.expect("end admin");
        });
    }

    #[test]
    fn cancelled_and_panicking_lease_owners_cannot_return_unfinished_sessions() {
        let server = Server::acquire();
        Runtime::new().expect("runtime").block_on(async {
            let name = server.database().await;
            let connections = server.connections(&name);
            for panicking in [false, true] {
                let lease = connections.lease().await.expect("write lease");
                lease
                    .client()
                    .batch_execute("BEGIN")
                    .await
                    .expect("begin write");
                let pid: i32 = lease
                    .client()
                    .query_one("SELECT pg_backend_pid()", &[])
                    .await
                    .expect("backend")
                    .get(0);
                let (ready, started) = oneshot::channel();
                let (fail, failure) = oneshot::channel();
                let task = tokio::spawn(async move {
                    let _lease = lease;
                    ready.send(()).expect("owner started");
                    failure.await.expect("trigger owner panic");
                    panic!("simulated write owner panic");
                });
                timeout(Duration::from_secs(1), started)
                    .await
                    .expect("owner start budget")
                    .expect("owner ready");
                if panicking {
                    fail.send(()).expect("request owner panic");
                } else {
                    task.abort();
                }
                let error = timeout(Duration::from_secs(1), task)
                    .await
                    .expect("owner end budget")
                    .expect_err("owner failed");
                assert_eq!(error.is_panic(), panicking);
                assert_eq!(error.is_cancelled(), !panicking);
                assert_eq!(connections.pool.status().size, 0);
                let replacement = connections.lease().await.expect("replacement lease");
                assert_ne!(
                    replacement
                        .client()
                        .query_one("SELECT pg_backend_pid()", &[])
                        .await
                        .expect("replacement backend")
                        .get::<_, i32>(0),
                    pid
                );
                replacement.discard().await.expect("end replacement driver");
            }
            timeout(Duration::from_secs(2), connections.close())
                .await
                .expect("close every driver");
        });
    }

    #[test]
    fn leases_require_confirmed_end_and_close_waits_for_every_driver() {
        let server = Server::acquire();
        Runtime::new().expect("runtime").block_on(async {
            let name = server.database().await;
            let connections = server.connections(&name);
            let lease = connections.lease().await.expect("write lease");
            let first: i32 = lease
                .client()
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .expect("backend")
                .get(0);
            lease
                .client()
                .batch_execute("BEGIN; COMMIT")
                .await
                .expect("known end");
            lease.release();
            let lease = connections.lease().await.expect("reused lease");
            assert_eq!(
                lease
                    .client()
                    .query_one("SELECT pg_backend_pid()", &[])
                    .await
                    .expect("backend")
                    .get::<_, i32>(0),
                first
            );
            lease
                .client()
                .batch_execute("BEGIN")
                .await
                .expect("unfinished write");
            drop(lease);
            let replacement = connections.lease().await.expect("replacement");
            assert_ne!(
                replacement
                    .client()
                    .query_one("SELECT pg_backend_pid()", &[])
                    .await
                    .expect("backend")
                    .get::<_, i32>(0),
                first
            );
            let occupied = connections.lease().await.expect("second online slot");
            let dedicated = connections
                .dedicated()
                .await
                .expect("independent clock slot");
            let recovery = connections
                .recover()
                .await
                .expect("independent recovery slot");
            let drivers = connections.pool.manager().0.drivers().clone();
            drop((replacement, occupied, dedicated, recovery));
            timeout(Duration::from_secs(2), connections.close())
                .await
                .expect("bounded closure");
            assert!(drivers.iter().all(|driver| driver.is_finished()));
            assert!(connections.acquire().await.is_err());
            assert!(connections.recover().await.is_err());
            assert!(connections.dedicated().await.is_err());
        });
    }
}
