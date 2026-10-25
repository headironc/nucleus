//! Application-owned startup, shutdown and instance lifetime.

mod retention;
mod transport;

use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};

use domain::{
    database::Barrier,
    identity::{authentication, challenge::client, message},
    runtime,
};
use platform::database::Connections;
use thiserror::Error;
use tokio::{
    signal::unix::{SignalKind, signal},
    time::{self, timeout_at},
};

use crate::{identity, keys, settings, source, suppliers};

pub const DEFAULT_STOP_SECONDS: u64 = 30;

/// The production entrypoint has no fixture endpoints or alternate credential sources.
pub fn run() -> Result<(), RunError> {
    tracing::info!(stage = "start", "starting online service");
    if std::env::var_os("NOTIFY_SOCKET").is_none() {
        return Err(RunError::NotifyMissing);
    }
    let settings = settings::Settings::load()?;
    let keys = keys::load()?;
    let connections = crate::database::load(settings.database)?;
    let (challenge, message) =
        suppliers::Clients::new(settings.challenge, settings.message)?.into_parts();
    let runtime = tokio::runtime::Runtime::new().map_err(RunError::Runtime)?;
    let stop_budget = settings.process.stop;
    runtime.block_on(supervise(
        Application::start(
            connections,
            keys,
            settings.services,
            settings.process,
            challenge,
            message,
        ),
        stop_budget,
        settings.listen,
        settings.source,
        settings.requests,
        notify,
    ))
}

async fn supervise(
    starting: impl std::future::Future<Output = Result<Application, Error>>,
    stop_budget: Duration,
    address: std::net::SocketAddr,
    source: source::Settings,
    requests: identity::Settings,
    ready: impl FnOnce(std::net::SocketAddr) -> Result<(), RunError>,
) -> Result<(), RunError> {
    let mut terminate = signal(SignalKind::terminate()).map_err(RunError::Signal)?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(RunError::Signal)?;
    tokio::pin!(starting);
    let stopped = async {
        tokio::select! {
            option = terminate.recv() => option.ok_or(RunError::SignalClosed),
            option = interrupt.recv() => option.ok_or(RunError::SignalClosed),
        }
    };
    tokio::pin!(stopped);
    let mut application = tokio::select! {
        biased;
        result = &mut stopped => {
            let deadline = time::Instant::now() + stop_budget;
            tracing::info!(stage = "start_stop", "stopping during startup verification");
            // Keep the start future and its instance session alive until termination or exit.
            let mut application = match timeout_at(deadline, &mut starting).await {
                Ok(result) => result?,
                Err(_) => {
                    let error = RunError::StopTimeout { cause: result.err().map(Box::new) };
                    tracing::error!(stage = "start_stop", kind = "timeout", error = ?error);
                    std::process::exit(1);
                }
            };
            let result = application.finish(result, deadline).await;
            require_completion(&result);
            return result;
        }
        result = &mut starting => result?,
    };
    let result = application
        .serve(address, source, requests, &mut stopped, ready)
        .await;
    require_completion(&result);
    result
}

fn require_completion(result: &Result<(), RunError>) {
    // Do not unwind the application or its instance session before unfinished work exits.
    if let Err(error) = result
        && error.incomplete()
    {
        tracing::error!(stage = "stop", kind = "incomplete", error = ?error);
        std::process::exit(1);
    }
}

fn notify(_: std::net::SocketAddr) -> Result<(), RunError> {
    sd_notify::notify(&[sd_notify::NotifyState::Ready]).map_err(RunError::Notify)
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error("{0}")]
    Settings(#[from] settings::Error),
    #[error("{0}")]
    Keys(#[from] keys::Error),
    #[error("{0}")]
    Database(#[from] crate::database::Error),
    #[error("{0}")]
    Suppliers(#[from] suppliers::Error),
    #[error("{0}")]
    Application(#[from] Error),
    #[error("cannot construct the process runtime")]
    Runtime(#[source] io::Error),
    #[error("cannot register process signals")]
    Signal(#[source] io::Error),
    #[error("process signal stream ended")]
    SignalClosed,
    #[error("systemd readiness notification is unavailable")]
    NotifyMissing,
    #[error("shared admission closed before readiness")]
    Closed,
    #[error("cannot publish process readiness")]
    Notify(#[source] io::Error),
    #[error("HTTP listener failed")]
    Listener(#[source] io::Error),
    #[error("cannot serialize OpenAPI")]
    OpenApi(#[from] serde_json::Error),
    #[error("cannot construct HTTP ingress budgets")]
    Admission(#[source] identity::admission::Error),
    #[error("{0}")]
    Http(#[from] transport::Error),
    #[error("{0}")]
    Retention(#[from] retention::Error),
    #[error("HTTP service ended before shutdown")]
    Ended,
    #[error("retention service ended before shutdown")]
    RetentionEnded,
    #[error("application serving lifecycle is invalid")]
    Lifecycle,
    #[error("the common process stop budget expired")]
    StopTimeout { cause: Option<Box<RunError>> },
    #[error("process operation and application termination failed")]
    Combined {
        cause: Box<RunError>,
        termination: Error,
    },
}

impl RunError {
    /// Locate a process failure without formatting settings, inputs or source chains.
    pub fn stage(&self) -> &'static str {
        match self {
            Self::Settings(_) => "settings",
            Self::Application(Error::Instance(_)) => "instance_lock",
            Self::Keys(_) => "keys",
            Self::Database(_) => "database_assembly",
            Self::Suppliers(_) => "suppliers",
            Self::Application(Error::Database(_)) => "database_verification",
            Self::Application(Error::Services(_)) => "services",
            Self::Application(Error::Runtime(_)) | Self::Runtime(_) => "runtime",
            Self::Application(
                Error::Timeout { .. } | Error::Combined { .. } | Error::Shutdown(_),
            )
            | Self::Lifecycle => "application",
            Self::Application(Error::Http(_)) => "http",
            Self::Application(Error::Retention(_)) | Self::Retention(_) | Self::RetentionEnded => {
                "retention"
            }
            Self::Signal(_) | Self::SignalClosed => "signals",
            Self::NotifyMissing | Self::Notify(_) | Self::Closed => "readiness",
            Self::Listener(_) | Self::OpenApi(_) | Self::Admission(_) => "http_assembly",
            Self::Http(_) | Self::Ended => "http",
            Self::StopTimeout { .. } | Self::Combined { .. } => "stop",
        }
    }

    fn incomplete(&self) -> bool {
        matches!(
            self,
            Self::StopTimeout { .. }
                | Self::Application(Error::Timeout { .. })
                | Self::Combined {
                    termination: Error::Timeout { .. },
                    ..
                }
        )
    }
}

pub struct Settings {
    stop: Duration,
}

impl Settings {
    pub fn new(stop: Duration) -> Result<Self, SettingsError> {
        if stop.is_zero() || Instant::now().checked_add(stop).is_none() {
            return Err(SettingsError);
        }
        Ok(Self { stop })
    }
}

#[derive(Debug, Error)]
#[error("application stop budget must be positive and representable")]
pub struct SettingsError;

/// Owned by the entrypoint, which drains the application before dropping it.
pub struct Application {
    context: runtime::Context,
    services: domain::startup::Services,
    settings: Settings,
    phase: Phase,
    // Timeout reports share completed failures with subsequent stop attempts.
    failures: Vec<Arc<Error>>,
}

impl Application {
    /// Serve HTTP until `stopped` completes, then drain; `ready` receives the bound address.
    pub async fn serve(
        &mut self,
        address: std::net::SocketAddr,
        source: source::Settings,
        settings: identity::Settings,
        stopped: impl std::future::Future<Output = Result<(), RunError>>,
        ready: impl FnOnce(std::net::SocketAddr) -> Result<(), RunError>,
    ) -> Result<(), RunError> {
        if !matches!(self.phase, Phase::Prepared) {
            return Err(RunError::Lifecycle);
        }
        tokio::pin!(stopped);
        let prepared = tokio::select! {
            biased;
            result = &mut stopped => {
                return self.finish(result, time::Instant::now() + self.settings.stop).await;
            }
            result = transport::Transport::bind(address, self.services.clone(), settings, source) => result,
        };
        let transport = match prepared {
            Ok(transport) => transport,
            Err(cause) => {
                return self
                    .finish(Err(cause), time::Instant::now() + self.settings.stop)
                    .await;
            }
        };
        self.phase = Phase::Running {
            transport,
            retention: retention::Retention::start(self.services.clone()),
        };
        if let Err(error) = self.services.confirm().await {
            let deadline = time::Instant::now() + self.settings.stop;
            return self
                .finish(Err(RunError::Application(Error::Services(error))), deadline)
                .await;
        }
        let services = self.services.clone();
        // Never announce readiness after the instance session or another cause closed admission.
        let ready = move |address| {
            if !services.admitted() {
                return Err(RunError::Closed);
            }
            ready(address)
        };
        let result = self.phase.wait(&mut stopped, ready).await;
        let deadline = time::Instant::now() + self.settings.stop;
        self.finish(result, deadline).await
    }

    async fn finish(
        &mut self,
        result: Result<(), RunError>,
        deadline: time::Instant,
    ) -> Result<(), RunError> {
        tracing::info!(stage = "stop", "finishing application");
        match (result, self.stop_at(deadline).await) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(cause), Ok(())) => Err(cause),
            (Ok(()), Err(cause)) => Err(cause.into()),
            (Err(cause), Err(termination)) => Err(RunError::Combined {
                cause: Box::new(cause),
                termination,
            }),
        }
    }

    /// Refuse to start while another application or deployment command holds the
    /// database instance lock. The trusted-time session holds it until the drain ends
    /// every connection, or until the process exits after an incomplete stop.
    pub async fn start(
        connections: Connections,
        keys: domain::keys::Keys,
        services: domain::startup::Settings,
        settings: Settings,
        supplier: client::Client,
        message: message::client::Client,
    ) -> Result<Self, Error> {
        let deadline = time::Instant::now() + Duration::from_secs(10);
        let context = runtime::Context::hold(connections)
            .await
            .map_err(Error::Instance)?;
        let cause = match async {
            let barrier =
                timeout_at(deadline, Barrier::begin_at(context.clone(), deadline)).await??;
            Ok::<_, Error>(
                domain::startup::Services::start(barrier, keys, services, supplier, message)
                    .await?,
            )
        }
        .await
        {
            // The instance session can end during the verification scans.
            Ok(services) => match services.confirm().await {
                Ok(()) => {
                    return Ok(Self {
                        context,
                        services,
                        settings,
                        phase: Phase::Prepared,
                        failures: Vec::new(),
                    });
                }
                Err(error) => Error::Services(error),
            },
            Err(error) => error,
        };
        match context.stop().await {
            Ok(()) => Err(cause),
            Err(termination) => Err(Error::Combined {
                cause: Box::new(cause),
                termination,
            }),
        }
    }

    pub fn authentication(&self) -> &authentication::Service {
        self.services.authentication()
    }

    pub fn registration(&self) -> &domain::registration::Service {
        self.services.registration()
    }

    /// A completed drain ends the instance session last, which releases the lock.
    /// On timeout the lock stays held; the caller must exit instead of reporting a successful drain.
    pub async fn stop(&mut self) -> Result<(), Error> {
        self.stop_at(time::Instant::now() + self.settings.stop)
            .await
    }

    async fn stop_at(&mut self, deadline: time::Instant) -> Result<(), Error> {
        if matches!(self.phase, Phase::Stopped) {
            return Ok(());
        }
        if let Phase::Running {
            transport,
            retention,
        } = &self.phase
        {
            retention.close();
            transport.close();
        }
        self.services.close();
        match timeout_at(deadline, async {
            if let Phase::Running {
                transport,
                retention,
            } = &mut self.phase
            {
                if let Err(error) = transport.drain().await {
                    self.failures.push(Arc::new(error.into()));
                }
                if let Err(error) = retention.drain().await {
                    self.failures.push(Arc::new(error.into()));
                }
            }
            if let Err(error) = self.services.drain().await {
                self.failures.push(Arc::new(error.into()));
            }
            if let Err(error) = self.context.stop().await {
                self.failures.push(Arc::new(error.into()));
            }
            self.phase = Phase::Stopped;
            let failures = std::mem::take(&mut self.failures);
            if !failures.is_empty() {
                return Err(Error::Shutdown(failures));
            }
            tracing::info!(stage = "stopped", "application work and connections ended");
            Ok(())
        })
        .await
        {
            Ok(result) => result,
            Err(elapsed) => {
                // A stop signal can win supervision before a retention failure arrives.
                // Read only a completed task so this does not extend the stop budget.
                if let Phase::Running { retention, .. } = &mut self.phase
                    && retention.finished()
                    && let Err(error) = retention.wait().await
                {
                    self.failures.push(Arc::new(error.into()));
                }
                Err(Error::Timeout {
                    elapsed,
                    failures: self.failures.clone(),
                })
            }
        }
    }
}

enum Phase {
    Prepared,
    Running {
        transport: transport::Transport,
        retention: retention::Retention,
    },
    Stopped,
}

impl Phase {
    async fn wait(
        &mut self,
        stopped: impl std::future::Future<Output = Result<(), RunError>>,
        ready: impl FnOnce(std::net::SocketAddr) -> Result<(), RunError>,
    ) -> Result<(), RunError> {
        match self {
            Self::Prepared | Self::Stopped => Err(RunError::Lifecycle),
            Self::Running {
                transport,
                retention,
            } => tokio::select! {
                biased;
                // Observe a completed retention failure before transport can publish readiness.
                result = retention.wait() => {
                    result?;
                    Err(RunError::RetentionEnded)
                },
                result = transport.wait(stopped, ready) => result,
            },
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Http(#[from] transport::Error),
    #[error("{0}")]
    Retention(#[from] retention::Error),
    #[error("application resources failed during shutdown")]
    Shutdown(Vec<Arc<Error>>),
    #[error("{0}")]
    Database(#[from] domain::database::Error),
    #[error("{0}")]
    Services(#[from] domain::startup::Error),
    #[error("{0}")]
    Runtime(#[from] runtime::Error),
    #[error("{0}")]
    Instance(#[source] runtime::Error),
    #[error("application lifecycle budget expired")]
    Timeout {
        #[source]
        elapsed: tokio::time::error::Elapsed,
        failures: Vec<Arc<Error>>,
    },
    #[error("application lifecycle and resource termination failed")]
    Combined {
        cause: Box<Error>,
        termination: runtime::Error,
    },
}

impl From<tokio::time::error::Elapsed> for Error {
    fn from(elapsed: tokio::time::error::Elapsed) -> Self {
        Self::Timeout {
            elapsed,
            failures: Vec::new(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {

    use std::{
        sync::{
            Arc, Mutex, Weak,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{Router, body::Bytes, http::header, routing::post};
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use domain::{
        identity::{
            authentication::{Error, Input},
            challenge::{self, client},
        },
        keys::{Entry, Id, Material},
        startup::Settings,
    };
    use nonempty::NonEmpty;
    use platform::database::{self, Connections, credentials::Password};
    use rustls::RootCertStore;
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, BufReader},
        net::TcpListener,
        runtime::Runtime,
        task::JoinHandle,
        time::timeout,
    };
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use crate::{
        diagnostics::tests::contains_text,
        process::{self, Application},
    };

    static SERVER: Mutex<Weak<fixture::Server>> = Mutex::new(Weak::new());

    async fn message() -> domain::identity::message::client::Client {
        let supplier = fixture::supplier::Supplier::start().await;
        message_at(supplier.address(), supplier.authority())
    }

    fn message_at(
        address: std::net::SocketAddr,
        authority: &[u8],
    ) -> domain::identity::message::client::Client {
        use domain::identity::message::{
            client::Client,
            credentials::{AccessKey, Id, Secret},
            settings::{Settings, Signature, TemplateCode, Templates},
        };
        Client::for_test(
            address,
            reqwest::Certificate::from_der(authority).expect("fixture certificate"),
            Settings::new(
                Signature::new("测试".into()).expect("signature"),
                Templates::new(
                    TemplateCode::new("auth".into()).expect("template"),
                    TemplateCode::new("change".into()).expect("template"),
                    TemplateCode::new("bind".into()).expect("template"),
                ),
                1000,
            )
            .expect("settings"),
            AccessKey::new(
                Id::new("FictionalAccessKey".into()).expect("id"),
                Secret::new("fictional-secret".into()).expect("secret"),
            ),
        )
        .expect("unused loopback client")
    }

    pub(super) struct Supplier {
        port: u16,
        task: JoinHandle<()>,
        calls: Arc<AtomicUsize>,
    }

    impl Supplier {
        async fn echoing() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("loopback captcha");
            let port = listener.local_addr().expect("address").port();
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = Arc::clone(&calls);
            let router=Router::new().route("/siteverify",post(move |body:Bytes| {
                let observed=Arc::clone(&observed);
                async move {
                    observed.fetch_add(1,Ordering::SeqCst);
                    let input:serde_json::Value=serde_json::from_slice(&body).expect("captcha JSON");
                    let token=input["response"].as_str().expect("fixture operation token");
                    let (action, operation) = if let Some(operation) = token.strip_prefix("mobile:") {
                        ("send_mobile_change_code", operation)
                    } else if let Some(operation) = token.strip_prefix("binding:") {
                        ("send_mobile_binding_code", operation)
                    } else {
                        ("send_authentication_code", token)
                    };
                    axum::Json(serde_json::json!({"success":true,"challenge_ts":chrono::Utc::now(),"hostname":"example.com","action":action,"cdata":operation}))
                }
            }));
            let task = tokio::spawn(async move {
                axum::serve(listener, router).await.expect("captcha server");
            });
            Self { port, task, calls }
        }

        pub(super) async fn new() -> Self {
            Self::responding(r#"{"success":false,"error-codes":["invalid-input-response"]}"#.into())
                .await
        }

        async fn responding(body: String) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("local supplier");
            let port = listener.local_addr().expect("supplier address").port();
            let calls = Arc::new(AtomicUsize::new(0));
            let observed = Arc::clone(&calls);
            let body = Bytes::from(body);
            let router = Router::new().route(
                "/siteverify",
                post(move |_: Bytes| {
                    let observed = Arc::clone(&observed);
                    let body = body.clone();
                    async move {
                        observed.fetch_add(1, Ordering::SeqCst);
                        (
                            [
                                (header::CONTENT_TYPE, "application/json"),
                                (header::CONNECTION, "close"),
                            ],
                            body,
                        )
                    }
                }),
            );
            let task = tokio::spawn(async move {
                axum::serve(listener, router)
                    .await
                    .expect("fixture supplier");
            });
            Self { port, task, calls }
        }

        fn client(&self) -> client::Client {
            client::Client::for_test(
                self.port,
                challenge::settings::Settings::new(
                    challenge::settings::AllowedHostnames::new(NonEmpty::new("example.com".into()))
                        .expect("hostnames"),
                ),
                "fictional-secret".parse().expect("secret"),
            )
            .expect("fixture client")
        }
    }

    impl Drop for Supplier {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn connections(server: &fixture::Server, name: &str, user: &str) -> Connections {
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

    pub(super) fn server() -> Arc<fixture::Server> {
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

    /// Two fictional root versions; `active` selects the one every use derives from.
    fn keys(active: i64) -> domain::keys::Keys {
        let entries = (0..2)
            .map(|id| {
                Entry::new(
                    Id::new(id).expect("key id"),
                    Material::new(Zeroizing::new(
                        URL_SAFE_NO_PAD.encode([1 + u8::try_from(id).expect("key byte"); 32]),
                    ))
                    .expect("fictional material"),
                )
            })
            .collect();
        domain::keys::Keys::new(Id::new(active).expect("active"), entries).expect("root versions")
    }

    fn session_settings() -> domain::identity::authentication::session::settings::Settings {
        domain::identity::authentication::session::settings::Settings::new(
            "website-fixture".parse().expect("app"),
            86400,
            600,
            600,
        )
        .expect("session settings")
    }

    fn avatar_settings() -> domain::identity::avatar::link::Link {
        domain::identity::avatar::link::Link::new(
            "https://avatars.example.com".into(),
            Duration::from_secs(86400),
        )
        .expect("avatar settings")
    }

    async fn empty_database(server: &fixture::Server) -> String {
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

    async fn initialize(
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

    pub(super) async fn database(server: &fixture::Server) -> String {
        let name = empty_database(server).await;
        initialize(server, &name, "13900000000").await;
        name
    }

    /// Probe the database instance lock from an unrelated session without waiting for it.
    /// Probe the database instance lock from an unrelated session without waiting for it.
    /// "Held" is immediate; a probe that took the lock waits until the server released it,
    /// so the probe itself never blocks a later start.
    pub(super) async fn instance_held(server: &fixture::Server, name: &str) -> bool {
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
    pub(super) async fn instance_released(server: &fixture::Server, name: &str) -> bool {
        let observer = connections(server, name, "postgres")
            .recover()
            .await
            .expect("instance lock observer");
        let released = timeout(Duration::from_secs(10), async {
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
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok();
        observer.terminate().await.expect("observer ended");
        released
    }

    pub(super) async fn start(
        server: &fixture::Server,
        name: &str,
        active: i64,
        capacity: usize,
        supplier: &Supplier,
    ) -> Application {
        Application::start(
            connections(server, name, "clerk"),
            keys(active),
            Settings::new(
                capacity,
                domain::identity::message::Limits::default(),
                session_settings(),
                domain::registration::Limits::default(),
                avatar_settings(),
            )
            .expect("settings"),
            process::Settings::new(Duration::from_secs(30)).expect("process settings"),
            supplier.client(),
            message().await,
        )
        .await
        .expect("start acceptance")
    }

    fn input(operation_id: &str, mobile: &str, token: &str) -> Input {
        Input::new(
            operation_id.parse().expect("operation"),
            mobile.parse().expect("mobile"),
            token.parse().expect("token"),
            "::ffff:192.0.2.1".parse().expect("source"),
        )
    }

    const OPERATION: &str = "00000000-0000-0000-0000-000000000000";
    const OTHER: &str = "ffffffff-ffff-ffff-ffff-ffffffffffff";

    async fn running(
        mut application: Application,
    ) -> (
        String,
        tokio::sync::oneshot::Sender<()>,
        JoinHandle<(Application, Result<(), process::RunError>)>,
    ) {
        let (ready, address) = tokio::sync::oneshot::channel();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let settings =
            crate::identity::Settings::new("https://example.com".into(), "fixture-site-key".into())
                .expect("origin settings");
        let source =
            crate::source::Settings::new(vec!["127.0.0.0/8".parse().expect("fixture proxy")])
                .expect("source settings");
        let task = tokio::spawn(async move {
            let result = application
                .serve(
                    "127.0.0.1:0".parse().expect("HTTP listener"),
                    source,
                    settings,
                    async {
                        match stopped.await {
                            Ok(()) => Ok(()),
                            Err(_) => Err(process::RunError::SignalClosed),
                        }
                    },
                    |address| {
                        ready.send(address).expect("announce bound listener");
                        Ok(())
                    },
                )
                .await;
            (application, result)
        });
        let address = timeout(Duration::from_secs(5), address)
            .await
            .expect("HTTP readiness budget")
            .expect("bound listener");
        (format!("http://{address}"), stop, task)
    }

    fn http_client() -> reqwest::Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "origin",
            reqwest::header::HeaderValue::from_static("https://example.com"),
        );
        headers.insert(
            "x-real-ip",
            reqwest::header::HeaderValue::from_static("192.0.2.1"),
        );
        reqwest::Client::builder()
            .default_headers(headers)
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("fixture HTTP client")
    }

    fn accept_sms(request: fixture::supplier::Request) {
        let text = std::str::from_utf8(&request.bytes).expect("fictional request bytes");
        let target = text
            .lines()
            .next()
            .expect("request line")
            .split(' ')
            .nth(1)
            .expect("request target");
        let url = url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}"))
            .expect("supplier URL");
        let message_id = url
            .query_pairs()
            .find(|(name, _)| name == "OutId")
            .expect("original OutId")
            .1
            .into_owned();
        let body = serde_json::json!({"Code":"OK","Success":true,"Model":{"OutId":message_id,"BizId":"fictional-biz","RequestId":"57fdfbd7-e79d-465a-a121-eab3e91e7290"}}).to_string();
        request
            .reply
            .send(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .into_bytes(),
            )
            .expect("supplier acceptance");
    }

    async fn issued_session_for(
        base: &str,
        sms: &mut fixture::supplier::Supplier,
        mobile: &str,
    ) -> (String, String) {
        issued_session_from(base, sms, mobile, "198.51.100.90").await
    }

    async fn issued_session_from(
        base: &str,
        sms: &mut fixture::supplier::Supplier,
        mobile: &str,
        source: &str,
    ) -> (String, String) {
        issued_session_with_identity(base, sms, mobile, source, "00000020000229007X").await
    }

    async fn issued_session_with_identity(
        base: &str,
        sms: &mut fixture::supplier::Supplier,
        mobile: &str,
        source: &str,
        id_number: &str,
    ) -> (String, String) {
        issued_session_and_registration(base, sms, mobile, source, id_number)
            .await
            .0
    }

    struct RegistrationReplay {
        operation: Uuid,
        input: serde_json::Value,
    }

    async fn issued_session_and_registration(
        base: &str,
        sms: &mut fixture::supplier::Supplier,
        mobile: &str,
        source: &str,
        id_number: &str,
    ) -> ((String, String), Option<RegistrationReplay>) {
        let client = http_client();
        let operation = Uuid::new_v4().to_string();
        let mut request = tokio::spawn(
            client
                .post(format!("{base}/api/v1/codes"))
                .header("operation-id", &operation)
                .header("x-real-ip", source)
                .json(&serde_json::json!({"mobile":mobile,"token":operation}))
                .send(),
        );
        let sent = tokio::select! {
            response = &mut request => {
                let response = response.expect("code request task").expect("code response");
                let status = response.status();
                let body = response.json::<serde_json::Value>().await.expect("code error");
                panic!("code request completed before SMS: {status}, {}", body["code"]);
            }
            result = timeout(Duration::from_secs(3), sms.request()) => result.expect("actual SMS request"),
        };
        let text = std::str::from_utf8(&sent.bytes).expect("wire");
        let target = text
            .lines()
            .next()
            .expect("request line")
            .split(' ')
            .nth(1)
            .expect("target");
        let url = url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("URL");
        let template = url
            .query_pairs()
            .find(|(name, _)| name == "TemplateParam")
            .expect("template")
            .1
            .into_owned();
        let code =
            serde_json::from_str::<serde_json::Value>(&template).expect("template JSON")["code"]
                .as_str()
                .expect("code")
                .to_owned();
        accept_sms(sent);
        let response = request.await.expect("C1 caller").expect("C1 response");
        assert_eq!(response.status(), 200);
        let message = response.json::<serde_json::Value>().await.expect("message");
        let response=client.post(format!("{base}/api/v1/sessions")).header("operation-id",&operation).json(&serde_json::json!({"secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","credential":{"type":"message","id":message["message"]["id"],"mobile":mobile,"code":code}})).send().await.expect("R1");
        assert_eq!(response.status(), 200);
        let option = response.headers().get("set-cookie").cloned();
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("R1 result");
        if let Some(cookie) = option {
            return (
                (
                    body["session"]["id"].as_str().expect("id").into(),
                    cookie
                        .to_str()
                        .expect("cookie")
                        .split(';')
                        .next()
                        .expect("pair")
                        .into(),
                ),
                None,
            );
        }
        let replay = RegistrationReplay {
            operation: Uuid::new_v4(),
            input: serde_json::json!({"id":body["registration"]["id"],"token":body["registration"]["token"],"name":"Fixture Registered","idNumber":id_number}),
        };
        let response = client
            .post(format!("{base}/api/v1/registrations"))
            .header("operation-id", replay.operation.to_string())
            .json(&replay.input)
            .send()
            .await
            .expect("R2");
        assert_eq!(response.status(), 201);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .expect("cookie")
            .split(';')
            .next()
            .expect("pair")
            .to_owned();
        let body = response
            .json::<serde_json::Value>()
            .await
            .expect("R2 session");
        (
            (body["session"]["id"].as_str().expect("id").into(), cookie),
            Some(replay),
        )
    }

    async fn account_candidate(
        base: &str,
        manager: &(String, String),
        mobile: &str,
    ) -> serde_json::Value {
        let response = http_client()
            .post(format!("{base}/api/v1/accounts/candidates"))
            .header("operation-id", Uuid::new_v4().to_string())
            .header("session-id", &manager.0)
            .header("cookie", &manager.1)
            .header("x-real-ip", "203.0.118.1")
            .json(&serde_json::json!({"mobile":mobile}))
            .send()
            .await
            .expect("real account confirmation");
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("set-cookie"));
        response.json().await.expect("masked account candidate")
    }

    #[test]
    fn account_suspension_accepts_legally_cleaned_and_retained_session_sets() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(45),async {
                for hours in [49_i32,25_i32] {
                    let name=database(&server).await;
                    let supplier=Supplier::echoing().await;
                    let mut sms=fixture::supplier::Supplier::start().await;
                    let application=Application::start(connections(&server,&name,"clerk"),keys(0),Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("initial startup");
                    let (base,stop,task)=running(application).await;
                    let original=issued_session_for(&base,&mut sms,"13800000000").await;
                    let observer=connections(&server,&name,"clerk").recover().await.expect("runtime observer");
                    let target=observer.client().query_one("SELECT account.id, account.person_id, person.revision_id, holder.assignment_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id JOIN access.holders holder ON holder.id=account.id WHERE session.id=$1::text::uuid",&[&original.0]).await.expect("R2 identity before session cleanup");
                    let account_id:Uuid=target.get("id");
                    stop.send(()).expect("stop before historical-time fixture");let (application,result)=task.await.expect("initial server");result.expect("drained");let issuance=application.registration().clone();drop(application);
                    let mut owner=connections(&server,&name,"postgres").recover().await.expect("isolated time fixture owner");
                    issuance.age_for_test(owner.client_mut(), chrono::TimeDelta::hours(i64::from(hours))).await.expect("consistent history and holder seals");
                    owner.terminate().await.expect("owner ended");
                    let application=Application::start(connections(&server,&name,"clerk"),keys(0),Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("complete historical graph passes startup");
                    application.services.registration().retention().run().await.expect("real R2 cleanup before background starts");
                    let row=observer.client().query_one("SELECT (SELECT count(*) FROM identity.sessions WHERE account_id=$1) AS sessions, NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$2::text::uuid) AS no_handoff, EXISTS(SELECT 1 FROM identity.accounts WHERE id=$1) AND EXISTS(SELECT 1 FROM access.holders WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.receipts WHERE account_id=$1) AS retained, NOT EXISTS(SELECT 1 FROM identity.sessions WHERE account_id=$1 AND expires_at>clock_timestamp()) AS expired",&[&account_id,&original.0]).await.expect("legal cleanup effects");
                    assert_eq!(row.get::<_,i64>("sessions"),i64::from(hours==25));assert!(row.get::<_,bool>("no_handoff"));assert!(row.get::<_,bool>("retained"));assert!(row.get::<_,bool>("expired"));
                    let (base,stop,task)=running(application).await;
                    let manager=issued_session_for(&base,&mut sms,"13900000000").await;
                    let previous=if hours==25 {
                        let identity=issued_session_from(&base,&mut sms,"13800000000","198.51.100.91").await;
                        let operation=Uuid::new_v4();
                        let response=http_client().delete(format!("{base}/api/v1/sessions/current")).header("operation-id",operation.to_string()).header("session-id",&identity.0).header("cookie",&identity.1).send().await.expect("real earlier logout");
                        assert_eq!(response.status(),200);
                        let snapshot:String=observer.client().query_one("SELECT row_to_json(revocation)::text FROM identity.revocations revocation WHERE id=$1::text::uuid AND operation_id=$2",&[&identity.0,&operation]).await.expect("exact earlier revocation").get(0);
                        Some((identity.0,snapshot))
                    } else {None};
                    let candidate=account_candidate(&base,&manager,"13800000000").await;
                    assert_eq!(candidate["identity"]["accountId"],serde_json::json!(account_id));assert!(candidate["suspensionId"].is_null());
                    let operation=Uuid::new_v4();
                    let input=serde_json::json!({"personId":target.get::<_,Uuid>("person_id"),"revisionId":target.get::<_,Uuid>("revision_id"),"mobile":"13800000000"});
                    let request=|| http_client().post(format!("{base}/api/v1/accounts/{account_id}/suspensions")).header("operation-id",operation.to_string()).header("session-id",&manager.0).header("cookie",&manager.1).header("x-real-ip","203.0.119.1").json(&input);
                    let response=request().send().await.expect("suspension without active target sessions");assert_eq!(response.status(),201);assert!(!response.headers().contains_key("set-cookie"));
                    let result=response.json::<serde_json::Value>().await.expect("permanent result");
                    let sequence=if hours==25 {3_i64}else{2_i64};
                    let row=observer.client().query_one("SELECT suspension.id, suspension.suspended_at, suspension.sequence=$3 AND account.sequence=$3 AND person.sequence=1 AS advanced, holder.assignment_id=$2 AS unchanged_assignment, (SELECT count(*) FROM identity.revocations WHERE mode='suspension' AND cause_id=suspension.id AND operation_id=suspension.operation_id) AS revoked, NOT EXISTS(SELECT 1 FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=suspension.account_id) AS no_handoff FROM identity.suspensions suspension JOIN identity.accounts account ON account.id=suspension.account_id JOIN identity.persons person ON person.id=suspension.person_id JOIN access.holders holder ON holder.id=suspension.account_id WHERE suspension.operation_id=$1",&[&operation,&target.get::<_,Uuid>("assignment_id"),&sequence]).await.expect("independent durable suspension before replay");
                    assert!(row.get::<_,bool>("advanced"));assert!(row.get::<_,bool>("unchanged_assignment"));assert!(row.get::<_,bool>("no_handoff"));assert_eq!(row.get::<_,i64>("revoked"),i64::from(hours==25));
                    assert_eq!(result,serde_json::json!({"code":"ok","suspension":{"id":row.get::<_,Uuid>("id"),"suspendedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("suspended_at")}}));
                    if let Some((id,snapshot))=previous {
                        assert_eq!(observer.client().query_one("SELECT row_to_json(revocation)::text FROM identity.revocations revocation WHERE id=$1::text::uuid",&[&id]).await.expect("earlier logout retained").get::<_,String>(0),snapshot);
                        assert!(observer.client().query_one("SELECT revocation.mode='suspension' AND session.expires_at<revocation.revoked_at FROM identity.sessions session JOIN identity.revocations revocation ON revocation.id=session.id WHERE session.id=$1::text::uuid",&[&original.0]).await.expect("retained expired member revoked").get::<_,bool>(0));
                    }
                    let response=request().send().await.expect("original zero or retained result");assert_eq!(response.status(),201);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("same result"),result);
                    stop.send(()).expect("stop");let (application,result)=task.await.expect("server");result.expect("drained");drop(application);
                    assert_eq!(supplier.calls.load(Ordering::SeqCst),if hours==25 {3}else{2});assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no supplier request through cleanup, replay and drain");
                    observer.terminate().await.expect("observer ended");sms.stop().await;
                }
            }).await.expect("session-set HTTP acceptance budget");
        });
    }

    #[test]
    fn stop_retains_running_retention_and_finishes_only_its_current_candidate() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let mut application = start(&server, &name, 0, 8, &supplier).await;
                for (operation_id, token) in [(OPERATION, "fictional-token"), (OTHER, "other-token")] {
                    application.authentication().verify(input(operation_id, "13800000000", token))
                        .await.expect("retained verification result");
                }
                application.stop().await.expect("original process drained");
                drop(application);
                let admin = connections(&server, &name, "postgres").recover().await.expect("fixture admin");
                admin.client().batch_execute("BEGIN; ALTER TABLE identity.challenges DISABLE TRIGGER USER;
                    UPDATE identity.challenges SET created_at=created_at-interval '25 hours', expires_at=expires_at-interval '25 hours', finished_at=finished_at-interval '25 hours';
                    ALTER TABLE identity.challenges ENABLE TRIGGER USER; COMMIT").await.expect("expired fixture history");
                let mut application = start(&server, &name, 0, 8, &supplier).await;
                application.settings.stop = Duration::from_millis(30);
                admin.client().batch_execute("BEGIN; SELECT pg_advisory_xact_lock(1314210636, 1)").await.expect("hold retention transaction");
                let (_, stop, task) = running(application).await;
                timeout(Duration::from_secs(3), async {
                    loop {
                        let blocked: bool = admin.client().query_one(
                            "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND pg_backend_pid()=ANY(pg_blocking_pids(pid)))", &[]
                        ).await.expect("retention barrier observer").get(0);
                        if blocked { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("retention reached its transaction");
                stop.send(()).expect("stop while retention is running");
                let (mut application, result) = timeout(Duration::from_secs(2), task).await.expect("bounded stop").expect("process task");
                assert!(matches!(result, Err(process::RunError::Application(process::Error::Timeout { .. }))));
                assert!(matches!(application.phase, process::Phase::Running { .. }));
                assert!(instance_held(&server, &name).await);
                application.settings.stop = Duration::from_secs(5);
                assert!(timeout(Duration::from_millis(30), application.stop()).await.is_err(), "cancelling another stop waiter must retain the task");
                admin.client().batch_execute("COMMIT").await.expect("release original transaction");
                application.stop().await.expect("original retention completed");
                assert!(matches!(application.phase, process::Phase::Stopped));
                application.stop().await.expect("repeated stop");
                let rows = admin.client().query("SELECT operation_id::text FROM identity.challenges ORDER BY operation_id", &[]).await.expect("retained candidates");
                assert_eq!(rows.len(), 1, "only the in-flight candidate was deleted");
                assert_eq!(rows[0].get::<_, String>(0), OTHER);
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 2, "retention never calls the supplier");
                admin.terminate().await.expect("fixture admin ends");
                drop(application);
            }).await.expect("retention stop budget");
        });
    }

    #[test]
    fn ingress_capacity_failures_have_one_safe_diagnostic_per_http_request() {
        use std::{collections::HashSet, future::IntoFuture, net::Ipv4Addr, process::Stdio};

        use crate::identity::admission::{Admission, Error};

        if std::env::var_os("NUCLEUS_TEST_ADMISSION_DIAGNOSTICS").is_none() {
            Runtime::new().expect("parent runtime").block_on(async {
                let child = tokio::process::Command::new(std::env::current_exe().expect("test binary"))
                    .args(["--exact", "process::tests::ingress_capacity_failures_have_one_safe_diagnostic_per_http_request", "--nocapture"])
                    .env("NUCLEUS_TEST_ADMISSION_DIAGNOSTICS", "1")
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn().expect("isolated diagnostic process");
                let output = timeout(Duration::from_secs(45), child.wait_with_output())
                    .await.expect("diagnostic process budget").expect("diagnostic process output");
                let text = String::from_utf8(output.stderr).expect("UTF-8 diagnostics");
                assert!(output.status.success(), "{text}");
                let events: Vec<serde_json::Value> = text.lines()
                    .map(|line| serde_json::from_str(line).expect("JSON diagnostic")).collect();
                for secret in ["private-admission-token", "private-admission-signature", "private-admission-body", "__Host-nucleus-session", "198.18.0.1", "203.0.113.55", "?signature="] {
                    assert!(!events.iter().any(|event| contains_text(event, secret)), "private request material in diagnostics");
                }
                let responses: Vec<_> = events.iter().filter(|event| event["fields"]["stage"] == "http_response").collect();
                let errors: Vec<_> = events.iter().filter(|event| event["level"] == "ERROR").collect();
                assert_eq!(responses.len(), 11);
                assert_eq!(errors.len(), 9);
                let mut requests = HashSet::new();
                for (index, response) in responses.iter().enumerate() {
                    let span = &response["spans"][0];
                    let request_id = span["request_id"].as_str().expect("request context");
                    assert!(requests.insert(request_id));
                    let errors: Vec<_> = errors.iter().filter(|error| error["spans"][0]["request_id"] == request_id).collect();
                    if index == 1 || index == 2 {
                        assert_eq!(response["fields"]["status"], if index == 1 { 403 } else { 429 });
                        assert!(errors.is_empty(), "ordinary rejection is not a technical failure");
                        continue;
                    }
                    assert_eq!(response["fields"]["status"], 503);
                    assert_eq!(errors.len(), 1, "one final failure per rejected request");
                    let error = errors[0];
                    assert_eq!(error["fields"]["stage"], "http_admission");
                    assert_eq!(error["fields"]["kind"], "unavailable");
                    assert_eq!(error["fields"]["error"], if index == 0 { "Transfer(NoPermits)" } else { "Capacity" });
                    assert_eq!(error["spans"][0], *span);
                    if span["route"] == "/api/v1/avatars/{image}" {
                        assert!(span.get("operation_id").is_none(), "anonymous image has no operation identity");
                    } else {
                        assert_eq!(span["operation_id"], OPERATION);
                    }
                }
            });
            return;
        }
        crate::diagnostics::init().expect("online diagnostics");
        let server = server();
        Runtime::new().expect("HTTP runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let mut application = start(&server, &name, 0, 8, &supplier).await;
                let admission = Admission::with_frozen_clock().expect("shared ingress budgets");
                let (failure, _failures) = tokio::sync::mpsc::channel(1);
                let (stop, mut stopped) = tokio::sync::watch::channel(false);
                let listener = crate::listener::Listener::bind(
                    "127.0.0.1:0".parse().expect("loopback address"),
                    failure,
                    stopped.clone(),
                )
                .await
                .expect("bounded listener");
                let base = format!(
                    "http://{}",
                    axum::serve::Listener::local_addr(&listener).expect("listener address")
                );
                let authentication = crate::identity::Context::new(
                    application.services.clone(),
                    crate::identity::Settings::new(
                        "https://example.com".into(),
                        "fixture-site-key".into(),
                    )
                    .expect("website origin"),
                    crate::source::Settings::new(vec!["127.0.0.0/8".parse().expect("proxy")])
                        .expect("trusted proxy"),
                    admission.clone(),
                );
                let routes = crate::identity::router(authentication.clone())
                    .merge(crate::workforce::router(
                        application.services.number().clone(),
                        authentication.clone(),
                    ))
                    .merge(crate::access::router(
                        application.services.role().clone(),
                        application.services.appointment().clone(),
                        application.services.grant().clone(),
                        application.services.assignment().clone(),
                        authentication,
                    ));
                let router = crate::router::new(routes, utoipa_axum::router::OpenApiRouter::new())
                    .expect("actual identity router");
                let task = tokio::spawn(
                    axum::serve(
                        listener,
                        router.into_make_service_with_connect_info::<crate::source::Peer>(),
                    )
                    .with_graceful_shutdown(async move {
                        let _ = stopped.wait_for(|stopped| *stopped).await;
                    })
                    .into_future(),
                );
                let client = http_client();
                let source = "198.18.0.1".parse().expect("retained source");
                let image = format!(
                    "{base}/api/v1/avatars/invalid.webp?signature=private-admission-signature"
                );
                let permits: Vec<_> = (0..8)
                    .map(|_| admission.transfer().expect("real image permit"))
                    .collect();
                let response = client
                    .get(&image)
                    .header("x-real-ip", "198.18.0.1")
                    .header(
                        "cookie",
                        "__Host-nucleus-session-private=private-admission-token",
                    )
                    .send()
                    .await
                    .expect("ninth image transfer");
                assert_eq!(response.status(), 503);
                assert!(!response.headers().contains_key("retry-after"));
                assert_eq!(
                    response
                        .json::<serde_json::Value>()
                        .await
                        .expect("capacity failure"),
                    serde_json::json!({"code":"dependency_unavailable"})
                );
                drop(permits);
                assert_eq!(
                    client
                        .get(&image)
                        .header("x-real-ip", "198.18.0.1")
                        .send()
                        .await
                        .expect("permit released")
                        .status(),
                    403
                );
                for _ in 0..20 {
                    assert!(matches!(
                        admission.image(source),
                        Ok(()) | Err(Error::Limited(_))
                    ));
                }
                let Err(Error::Limited(_)) = admission.image(source) else {
                    panic!("image source must have exhausted its burst");
                };
                let response = client
                    .get(&image)
                    .header("x-real-ip", "198.18.0.1")
                    .send()
                    .await
                    .expect("ordinary image rate limit");
                assert_eq!(response.status(), 429);
                assert_eq!(response.headers()["retry-after"], "1");
                assert_eq!(
                    response
                        .json::<serde_json::Value>()
                        .await
                        .expect("rate limit"),
                    serde_json::json!({"code":"rate_limited"})
                );

                for value in 0..10_000_u32 {
                    let source = Ipv4Addr::from(0xc612_0000 + value).into();
                    for result in [
                        admission.accept(source),
                        admission.create_session(source),
                        admission.read(source),
                        admission.write(source),
                        admission.image(source),
                    ] {
                        assert!(
                            matches!(result, Ok(()) | Err(Error::Limited(_))),
                            "source allocation precedes total quota"
                        );
                    }
                }
                for (method, path) in [
                    (reqwest::Method::POST, "/api/v1/codes"),
                    (reqwest::Method::POST, "/api/v1/sessions"),
                    (reqwest::Method::POST, "/api/v1/registrations"),
                    (reqwest::Method::GET, "/api/v1/accounts/current"),
                    (reqwest::Method::DELETE, "/api/v1/sessions/current"),
                    (reqwest::Method::DELETE, "/api/v1/sessions"),
                    (reqwest::Method::GET, "/api/v1/revocations"),
                    (
                        reqwest::Method::GET,
                        "/api/v1/avatars/invalid.webp?signature=private-admission-signature",
                    ),
                ] {
                    let response = client
                        .request(method, format!("{base}{path}"))
                        .header("operation-id", OPERATION)
                        .header("x-real-ip", "203.0.113.55")
                        .header(
                            "cookie",
                            "__Host-nucleus-session-private=private-admission-token",
                        )
                        .header("authorization", "Bearer private-admission-token")
                        .body("private-admission-body")
                        .send()
                        .await
                        .expect("source capacity failure precedes credentials and body");
                    assert_eq!(response.status(), 503, "{path}");
                    assert!(!response.headers().contains_key("retry-after"));
                    assert_eq!(response.headers()["cache-control"], "no-store");
                    assert_eq!(
                        response
                            .json::<serde_json::Value>()
                            .await
                            .expect("public source failure"),
                        serde_json::json!({"code":"dependency_unavailable"})
                    );
                }
                stop.send(true).expect("stop diagnostic listener");
                task.await.expect("listener task").expect("HTTP drained");
                application.stop().await.expect("application stopped");
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 0);
            })
            .await
            .expect("HTTP diagnostic fixture budget");
        });
    }

    #[test]
    fn read_and_write_budgets_are_independent_and_shared_by_router_instances() {
        use std::future::IntoFuture;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let mut application = start(&server, &name, 0, 8, &supplier).await;
                let admission = crate::identity::admission::Admission::with_frozen_clock()
                    .expect("shared process budgets");
                let mut addresses = Vec::new();
                let mut servers = Vec::new();
                for _ in 0..2 {
                    let (failure, _failures) = tokio::sync::mpsc::channel(1);
                    let (stop, mut stopped) = tokio::sync::watch::channel(false);
                    let listener = crate::listener::Listener::bind(
                        "127.0.0.1:0".parse().expect("address"),
                        failure,
                        stopped.clone(),
                    )
                    .await
                    .expect("listener");
                    addresses.push(format!(
                        "http://{}",
                        axum::serve::Listener::local_addr(&listener).expect("bound address")
                    ));
                    let authentication = crate::identity::Context::new(
                        application.services.clone(),
                        crate::identity::Settings::new(
                            "https://example.com".into(),
                            "fixture-site-key".into(),
                        )
                        .expect("origin"),
                        crate::source::Settings::new(vec!["127.0.0.0/8".parse().expect("proxy")])
                            .expect("sources"),
                        admission.clone(),
                    );
                    let routes = crate::identity::router(authentication.clone())
                        .merge(crate::center::router(
                            application.services.affiliation().clone(),
                            authentication.clone(),
                        ))
                        .merge(crate::workforce::router(
                            application.services.number().clone(),
                            authentication.clone(),
                        ))
                        .merge(crate::access::router(
                            application.services.role().clone(),
                            application.services.appointment().clone(),
                            application.services.grant().clone(),
                            application.services.assignment().clone(),
                            authentication,
                        ));
                    let router =
                        crate::router::new(routes, utoipa_axum::router::OpenApiRouter::new())
                            .expect("router");
                    servers.push((
                        stop,
                        tokio::spawn(
                            axum::serve(
                                listener,
                                router.into_make_service_with_connect_info::<crate::source::Peer>(),
                            )
                            .with_graceful_shutdown(async move {
                                let _ = stopped.wait_for(|stopped| *stopped).await;
                            })
                            .into_future(),
                        ),
                    ));
                }
                let client = http_client();
                let mut accepted = 0_u128;
                for index in 0..8 {
                    let (method, path) = match index % 3 {
                        0 => (reqwest::Method::DELETE, "/api/v1/sessions/current"),
                        1 => (reqwest::Method::DELETE, "/api/v1/sessions"),
                        _ => (reqwest::Method::POST, "/api/v1/roles"),
                    };
                    let response = client
                        .request(method, format!("{}{path}", addresses[index % 2]))
                        .header("operation-id", OPERATION)
                        .send()
                        .await
                        .expect("shared write source");
                    if response.status() == 401 {
                        accepted += 1;
                    } else {
                        assert_eq!(response.status(), 429);
                        assert!(response.headers().contains_key("retry-after"));
                    }
                }
                assert_eq!(accepted, 4);
                let mut accepted = 0_u128;
                for index in 0..40 {
                    let (method, path) = match index % 3 {
                        0 => (reqwest::Method::DELETE, "/api/v1/sessions/current"),
                        1 => (reqwest::Method::DELETE, "/api/v1/sessions"),
                        _ => (reqwest::Method::POST, "/api/v1/roles"),
                    };
                    let response = client
                        .request(method, format!("{}{path}", addresses[index % 2]))
                        .header("operation-id", OPERATION)
                        .header("x-real-ip", format!("198.19.0.{}", index + 1))
                        .send()
                        .await
                        .expect("shared write process");
                    if response.status() == 401 {
                        accepted += 1;
                    } else {
                        assert_eq!(response.status(), 429);
                        assert!(response.headers().contains_key("retry-after"));
                    }
                }
                assert_eq!(accepted, 16);
                for (path, requests, burst, status) in [
                    ("/api/v1/accounts/current", 80, 40, 401),
                    ("/api/v1/avatars/invalid.webp", 40, 20, 403),
                ] {
                    let mut accepted = 0;
                    for index in 0..requests {
                        let path = if path == "/api/v1/accounts/current" {
                            match index % 4 {
                                0 => "/api/v1/accounts/current/role",
                                1 => "/api/v1/assignments",
                                2 => "/api/v1/affiliations",
                                _ => path,
                            }
                        } else {
                            path
                        };
                        let response = client
                            .get(format!("{}{path}", addresses[index % 2]))
                            .header("operation-id", OPERATION)
                            .send()
                            .await
                            .expect("alternating routers");
                        if response.status() == status {
                            accepted += 1;
                        } else {
                            assert_eq!(response.status(), 429);
                            assert!(response.headers().contains_key("retry-after"));
                        }
                    }
                    assert_eq!(accepted, burst);
                }
                for (path, remaining, status) in [
                    ("/api/v1/accounts/current", 60, 401),
                    ("/api/v1/avatars/invalid.webp", 80, 403),
                ] {
                    let mut accepted = 0_u128;
                    for index in 0..200 {
                        let path = if path == "/api/v1/accounts/current" {
                            match index % 4 {
                                0 => "/api/v1/accounts/current/role",
                                1 => "/api/v1/assignments",
                                2 => "/api/v1/affiliations",
                                _ => path,
                            }
                        } else {
                            path
                        };
                        let response = client
                            .get(format!("{}{path}", addresses[index % 2]))
                            .header("operation-id", OPERATION)
                            .header("x-real-ip", format!("198.18.0.{}", index + 1))
                            .send()
                            .await
                            .expect("distinct sources across both routers");
                        if response.status() == status {
                            accepted += 1;
                        } else {
                            assert_eq!(response.status(), 429);
                            assert!(response.headers().contains_key("retry-after"));
                        }
                    }
                    assert_eq!(accepted, remaining);
                }
                for path in ["/api/v1/codes", "/api/v1/registrations"] {
                    assert_eq!(
                        client
                            .post(format!("{}{path}", addresses[0]))
                            .header("operation-id", OPERATION)
                            .json(&serde_json::json!({}))
                            .send()
                            .await
                            .expect("read quotas do not debit writes")
                            .status(),
                        422
                    );
                }
                for (stop, task) in servers {
                    stop.send(true).expect("stop listener");
                    task.await.expect("listener task").expect("HTTP drain");
                }
                application.stop().await.expect("application stopped");
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 0);
            })
            .await
            .expect("independent HTTP read budget test");
        });
    }

    async fn terminate_instance(server: &fixture::Server, name: &str) {
        let admin = connections(server, name, "postgres")
            .recover()
            .await
            .expect("fixture admin");
        let terminated: bool = admin
            .client()
            .query_one(
                "SELECT pg_terminate_backend(pid) FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND classid = 1314210636 AND objid = 3 AND objsubid = 2 AND granted AND database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())",
                &[],
            )
            .await
            .expect("terminate the instance backend")
            .get(0);
        assert!(terminated);
        timeout(Duration::from_secs(5), async {
            while admin
                .client()
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname = current_database() AND pid IN (SELECT pid FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND classid = 1314210636 AND objid = 3 AND objsubid = 2))",
                    &[],
                )
                .await
                .expect("instance backend observation")
                .get::<_, bool>(0)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the instance backend ended");
        admin.terminate().await.expect("admin ended");
    }

    #[test]
    fn an_instance_session_lost_during_startup_scans_refuses_the_start() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let proxy = fixture::database_proxy::Proxy::start(&server).await;
                // Hold a retained-fact scan that runs after the trusted-time sample.
                proxy.arm(fixture::database_proxy::Fault::StalledIdentityRevisions);
                let starting = tokio::spawn({
                    let connections = proxy_connections(&name, &proxy);
                    let client = supplier.client();
                    let message = message().await;
                    async move {
                        Application::start(
                            connections,
                            keys(0),
                            Settings::new(
                                2,
                                domain::identity::message::Limits::default(),
                                session_settings(),
                                domain::registration::Limits::default(),
                                avatar_settings(),
                            )
                            .expect("settings"),
                            process::Settings::new(Duration::from_secs(30))
                                .expect("process settings"),
                            client,
                            message,
                        )
                        .await
                    }
                });
                timeout(Duration::from_secs(10), proxy.intercepted())
                    .await
                    .expect("startup reached the held scan");
                terminate_instance(&server, &name).await;
                proxy.release();
                let error = timeout(Duration::from_secs(15), starting)
                    .await
                    .expect("startup ends")
                    .expect("startup task")
                    .err()
                    .expect("a lost instance session refuses the start");
                assert!(
                    matches!(&error, process::Error::Combined { cause, .. }
                        if matches!(cause.as_ref(),
                            process::Error::Services(domain::startup::Error::Runtime(_)))),
                    "{error:?}"
                );
                assert!(instance_released(&server, &name).await);
                proxy.stop().await;
            })
            .await
            .expect("lost startup instance budget");
        });
    }

    #[test]
    fn an_instance_session_lost_before_serving_never_announces_readiness() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let mut application = start(&server, &name, 0, 2, &supplier).await;
                terminate_instance(&server, &name).await;
                let result = application
                    .serve(
                        "127.0.0.1:0".parse().expect("listener"),
                        crate::source::Settings::new(vec!["127.0.0.1/32".parse().expect("proxy")])
                            .expect("source"),
                        crate::identity::Settings::new(
                            "https://example.com".into(),
                            "fixture-site-key".into(),
                        )
                        .expect("origin"),
                        std::future::pending(),
                        |_| panic!("a closed application must never announce readiness"),
                    )
                    .await;
                // Retention may also report its closed work while stopping; the cause must
                // still be the refused service confirmation.
                let cause = match &result {
                    Err(process::RunError::Combined { cause, .. }) => Some(cause.as_ref()),
                    Err(error) => Some(error),
                    Ok(()) => None,
                };
                assert!(
                    matches!(
                        cause,
                        Some(process::RunError::Application(process::Error::Services(_)))
                    ),
                    "{result:?}"
                );
                assert!(!application.services.admitted());
                assert!(matches!(application.phase, process::Phase::Stopped));
                drop(application);
                assert!(instance_released(&server, &name).await);
            })
            .await
            .expect("lost serving instance budget");
        });
    }

    #[test]
    fn a_second_application_is_refused_and_a_lost_instance_session_closes_admission_and_readiness()
    {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(40), async {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let application = start(&server, &name, 0, 8, &supplier).await;
                // Separate connections stand for another host using the same database.
                let second = Application::start(
                    connections(&server, &name, "clerk"),
                    keys(0),
                    Settings::new(
                        8,
                        domain::identity::message::Limits::default(),
                        session_settings(),
                        domain::registration::Limits::default(),
                        avatar_settings(),
                    )
                    .expect("settings"),
                    process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                    supplier.client(),
                    message().await,
                )
                .await
                .err()
                .expect("a second application is refused");
                assert!(matches!(
                    second,
                    process::Error::Instance(domain::runtime::Error::InstanceHeld)
                ));
                assert_eq!(process::RunError::Application(second).stage(), "instance_lock");

                // Keep retention pending until HTTP rejection is observed; its fatal
                // closed-admission result may otherwise stop the listener first.
                let blocker = connections(&server, &name, "postgres")
                    .recover()
                    .await
                    .expect("retention blocker");
                blocker.client().batch_execute("BEGIN; LOCK TABLE identity.transfers IN ACCESS EXCLUSIVE MODE")
                    .await.expect("hold initial retention scan");
                let blocker_pid: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[])
                    .await.expect("blocker identity").get(0);
                let (base, stop, task) = running(application).await;
                let ready = || {
                    http_client()
                        .get(format!("{base}/health/ready"))
                        .header("operation-id", "00000000-0000-4000-a000-000000000002")
                        .send()
                };
                assert_eq!(ready().await.expect("readiness").status(), 200);
                let admin = connections(&server, &name, "postgres")
                    .recover()
                    .await
                    .expect("fixture admin");
                timeout(Duration::from_secs(5), async {
                    loop {
                        let waiting: bool = admin.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_catalog.pg_blocking_pids(pid)))", &[&blocker_pid])
                            .await.expect("exact retention waiter").get(0);
                        if waiting { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("retention reaches the held lock");
                let terminated: bool = admin
                    .client()
                    .query_one(
                        "SELECT pg_terminate_backend(pid) FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND classid = 1314210636 AND objid = 3 AND objsubid = 2 AND granted AND database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())",
                        &[],
                    )
                    .await
                    .expect("terminate the instance backend")
                    .get(0);
                assert!(terminated);
                timeout(Duration::from_secs(5), async {
                    while ready().await.expect("readiness").status() != 503 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("readiness closes with the instance session");
                let response = http_client()
                    .post(format!("{base}/api/v1/codes"))
                    .header("operation-id", OPERATION)
                    .json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"}))
                    .send()
                    .await
                    .expect("closed admission response");
                assert_eq!(response.status(), 503);
                assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
                assert_eq!(ready().await.expect("readiness").status(), 503, "nothing reopens it");
                stop.send(()).expect("stop");
                blocker.client().batch_execute("COMMIT").await.expect("release retention scan");
                blocker.terminate().await.expect("blocker ended");
                let (application, _) = timeout(Duration::from_secs(10), task)
                    .await
                    .expect("bounded drain")
                    .expect("HTTP task");
                drop(application);
                assert!(instance_released(&server, &name).await);
                admin.terminate().await.expect("admin ended");
            })
            .await
            .expect("instance loss budget");
        });
    }

    #[test]
    fn a_pending_stop_prevents_readiness_and_releases_the_lock_after_draining() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let mut application = start(&server, &name, 0, 8, &supplier).await;
            application
                .serve(
                    "127.0.0.1:0".parse().expect("listener"),
                    crate::source::Settings::new(vec!["127.0.0.1/32".parse().expect("proxy")])
                        .expect("source"),
                    crate::identity::Settings::new(
                        "https://example.com".into(),
                        "fixture-site-key".into(),
                    )
                    .expect("origin"),
                    std::future::ready(Ok(())),
                    |_| panic!("stopped process must never announce readiness"),
                )
                .await
                .expect("drains without serving");
            assert!(application.services.ready().await.is_err());
            assert!(instance_released(&server, &name).await);
            drop(application);
            assert!(instance_released(&server, &name).await);
        });
    }

    #[test]
    fn a_completed_retention_failure_prevents_readiness_and_drains_the_application() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(10), async {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let proxy = fixture::database_proxy::Proxy::start(&server).await;
                let mut application = proxy_service(&supplier, &name, &proxy).await;
                let transport = process::transport::Transport::bind(
                    "127.0.0.1:0".parse().expect("listener"),
                    application.services.clone(),
                    crate::identity::Settings::new(
                        "https://example.com".into(),
                        "fixture-site-key".into(),
                    )
                    .expect("origin"),
                    crate::source::Settings::new(vec!["127.0.0.1/32".parse().expect("proxy")])
                        .expect("source"),
                )
                .await
                .expect("HTTP transport");
                proxy.arm(fixture::database_proxy::Fault::RewoundCurrent);
                let retention = process::retention::Retention::start(application.services.clone());
                timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("retention observes rewound time");
                proxy.release();
                // Establish the failed-task boundary before polling the readiness callback.
                while !retention.finished() {
                    tokio::task::yield_now().await;
                }
                application.phase = process::Phase::Running {
                    transport,
                    retention,
                };
                let mut announced = false;
                let result = application
                    .phase
                    .wait(std::future::pending(), |_| {
                        announced = true;
                        Ok(())
                    })
                    .await;
                let error = application
                    .finish(result, tokio::time::Instant::now() + Duration::from_secs(3))
                    .await
                    .expect_err("failed retention prevents startup");
                assert!(
                    !announced,
                    "a failed application must not publish readiness"
                );
                assert!(matches!(
                    error,
                    process::RunError::Retention(process::retention::Error::Transfer(
                        domain::identity::transfer::Error::Runtime(
                            domain::runtime::Error::ClockInvalid
                        )
                    ))
                ));
                assert!(matches!(application.phase, process::Phase::Stopped));
                assert!(application.services.ready().await.is_err());
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 0);
                assert!(instance_released(&server, &name).await);
                drop(application);
                assert!(instance_released(&server, &name).await);
                proxy.stop().await;
            })
            .await
            .expect("startup retention failure budget");
        });
    }

    #[test]
    fn http_start_failures_close_admission_and_keep_the_instance_owned() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            for occupied in [true, false] {
                let name = database(&server).await;
                let mut application = start(&server, &name, 0, 8, &supplier).await;
                let listener = TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("occupied listener");
                let address = if occupied {
                    listener.local_addr().expect("address")
                } else {
                    "127.0.0.1:0".parse().expect("dynamic port")
                };
                let settings = crate::identity::Settings::new(
                    "https://example.com".into(),
                    "fixture-site-key".into(),
                )
                .expect("origin settings");
                let result = application
                    .serve(
                        address,
                        crate::source::Settings::new(vec!["127.0.0.1/32".parse().expect("proxy")])
                            .expect("source settings"),
                        settings,
                        std::future::pending(),
                        |_| {
                            Err(process::RunError::Notify(
                                std::io::ErrorKind::ConnectionRefused.into(),
                            ))
                        },
                    )
                    .await;
                if occupied {
                    assert!(matches!(result, Err(process::RunError::Listener(_))));
                } else {
                    assert!(matches!(result, Err(process::RunError::Notify(_))));
                }
                assert!(application.services.ready().await.is_err());
                assert!(instance_released(&server, &name).await);
                drop(application);
                assert!(instance_released(&server, &name).await);
            }
            assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn retention_failure_survives_http_drain_timeout_before_or_after_a_stop_signal() {
        use tokio::{io::AsyncWriteExt, net::TcpStream};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                for stopping in [false, true] {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let proxy = fixture::database_proxy::Proxy::start(&server).await;
                let mut application = proxy_service(&supplier, &name, &proxy).await;
                application.settings.stop = Duration::from_millis(300);
                let observer = connections(&server, &name, "postgres")
                    .recover().await.expect("instance observer");
                proxy.arm(fixture::database_proxy::Fault::RewoundCurrent);
                let (ready, address) = tokio::sync::oneshot::channel();
                let (stop, stopped) = tokio::sync::oneshot::channel();
                let (noticed, notice) = tokio::sync::oneshot::channel();
                let task = tokio::spawn(async move {
                    let result = application.serve(
                        "127.0.0.1:0".parse().expect("listener"),
                        crate::source::Settings::new(vec!["127.0.0.1/32".parse().expect("proxy")]).expect("source"),
                        crate::identity::Settings::new("https://example.com".into(), "fixture-site-key".into()).expect("origin"),
                        async {
                            stopped.await.expect("stop signal");
                            noticed.send(()).expect("signal observer");
                            Ok(())
                        },
                        |address| { ready.send(address).expect("HTTP address"); Ok(()) },
                    ).await;
                    (application, result)
                });
                let address = address.await.expect("HTTP readiness");
                timeout(Duration::from_secs(2), proxy.intercepted())
                    .await.expect("retention holds its time observation");

                // 100 Continue proves the handler has started waiting for this body's bytes.
                let mut reader = TcpStream::connect(address)
                    .await.expect("slow HTTP reader");
                reader.write_all(format!(
                    "POST /api/v1/codes HTTP/1.1\r\nHost: localhost\r\nOperation-Id: {OPERATION}\r\nOrigin: https://example.com\r\nX-Real-IP: 192.0.2.1\r\nContent-Type: application/json\r\nContent-Length: 80\r\nExpect: 100-continue\r\n\r\n"
                ).as_bytes()).await.expect("request headers");
                let expected = b"HTTP/1.1 100 Continue\r\n\r\n";
                let mut response = vec![0; expected.len()];
                timeout(Duration::from_secs(1), reader.read_exact(&mut response)).await
                    .expect("body reader budget").expect("continue response");
                assert_eq!(response, expected);

                if stopping {
                    stop.send(()).expect("stop before the retention failure");
                    notice.await.expect("supervisor received the stop signal");
                }
                proxy.release();
                let (mut application, result) = timeout(Duration::from_secs(2), task).await
                    .expect("bounded failed shutdown").expect("process task");
                let error = result.expect_err("retention failure and HTTP timeout");
                if stopping {
                    assert!(matches!(&error, process::RunError::Application(process::Error::Timeout { failures, .. })
                        if matches!(failures.as_slice(), [failure]
                            if matches!(failure.as_ref(), process::Error::Retention(process::retention::Error::Transfer(
                                domain::identity::transfer::Error::Runtime(domain::runtime::Error::ClockInvalid)))))));
                } else {
                    assert!(matches!(&error, process::RunError::Combined { cause, termination: process::Error::Timeout { .. } }
                    if matches!(cause.as_ref(), process::RunError::Retention(process::retention::Error::Transfer(
                        domain::identity::transfer::Error::Runtime(domain::runtime::Error::ClockInvalid))))));
                }
                assert!(error.incomplete());
                let diagnostic = format!("{error:?}");
                assert!(diagnostic.contains("ClockInvalid"));
                assert!(diagnostic.contains("Timeout"));
                assert!(!diagnostic.contains("RetentionEnded"));
                // A business observation does not own the instance session. HTTP drain must
                // finish before the final high-water persistence and instance release.
                assert!(observer.client().query_one(
                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND classid = 1314210636 AND objid = 3 AND objsubid = 2 AND granted AND database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database()))",
                    &[],
                ).await.expect("instance remains held during HTTP drain").get::<_, bool>(0));
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 0);

                drop(reader);
                application.settings.stop = Duration::from_secs(5);
                let result = application.stop().await;
                if stopping {
                    assert!(matches!(result, Err(process::Error::Shutdown(ref failures)) if failures.len() == 1));
                } else {
                    result.expect("drain without reporting the delivered supervision failure again");
                }
                assert!(matches!(application.phase, process::Phase::Stopped));
                assert!(instance_released(&server, &name).await);
                drop(application);
                assert!(instance_released(&server, &name).await);
                observer.terminate().await.expect("observer ended");
                proxy.stop().await;
                }
            }).await.expect("retention failure test budget");
        });
    }

    #[test]
    fn stopping_preserves_a_fatal_retention_record_failure() {
        use std::{future::Future, task::Poll};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let mut application = start(&server, &name, 0, 8, &supplier).await;
                application.authentication().verify(input(OPERATION, "13800000000", "fictional-token"))
                    .await.expect("retained verification result");
                application.stop().await.expect("original process drained");
                drop(application);
                let admin = connections(&server, &name, "postgres").recover().await.expect("fixture admin");
                admin.client().batch_execute("BEGIN; ALTER TABLE identity.challenges DISABLE TRIGGER USER;
                    UPDATE identity.challenges SET created_at=created_at-interval '25 hours', expires_at=expires_at-interval '25 hours', finished_at=finished_at-interval '25 hours';
                    ALTER TABLE identity.challenges ENABLE TRIGGER USER; COMMIT").await.expect("expired fixture history");
                let mut application = start(&server, &name, 0, 8, &supplier).await;
                let transport = process::transport::Transport::bind(
                    "127.0.0.1:0".parse().expect("listener"),
                    application.services.clone(),
                    crate::identity::Settings::new("https://example.com".into(), "fixture-site-key".into()).expect("origin"),
                    crate::source::Settings::new(vec!["127.0.0.1/32".parse().expect("proxy")]).expect("source"),
                ).await.expect("HTTP transport");
                admin.client().batch_execute("BEGIN; SELECT pg_advisory_xact_lock(1314210636, 1)")
                    .await.expect("hold retention transaction");
                application.phase = process::Phase::Running {
                    transport,
                    retention: process::retention::Retention::start(application.services.clone()),
                };
                wait_for_lock(admin.client()).await;
                let mut stopping = Box::pin(application.stop());
                std::future::poll_fn(|context| {
                    assert!(stopping.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                }).await;
                admin.client().batch_execute("ALTER TABLE identity.challenges DISABLE TRIGGER USER;
                    UPDATE identity.challenges SET created_at=created_at+interval '27 hours', expires_at=expires_at+interval '27 hours', finished_at=finished_at+interval '27 hours';
                    ALTER TABLE identity.challenges ENABLE TRIGGER USER; COMMIT").await.expect("publish corrupt facts and release current cleanup");
                let error = stopping.await.expect_err("fatal retention cannot become successful shutdown");
                assert!(matches!(&error, process::Error::Shutdown(failures)
                    if matches!(failures.as_slice(), [failure]
                        if matches!(failure.as_ref(), process::Error::Retention(process::retention::Error::Operation(
                            Error::Unknown { cause, recovery }
                        )) if matches!(cause.as_ref(), Error::Time { stage: domain::identity::authentication::Stage::ChallengeHistory, .. })
                            && matches!(recovery.as_ref(), Error::Time { stage: domain::identity::authentication::Stage::ChallengeHistory, .. })))));
                assert!(matches!(application.phase, process::Phase::Stopped));
                let count: i64 = admin.client().query_one("SELECT count(*) FROM identity.challenges", &[])
                    .await.expect("retained evidence").get(0);
                assert_eq!(count, 1);
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 1, "retention must not call the supplier");
                assert!(instance_released(&server, &name).await);
                drop(application);
                assert!(instance_released(&server, &name).await);
                admin.terminate().await.expect("fixture admin ended");
            }).await.expect("fatal retention stop budget");
        });
    }

    #[test]
    fn cancelled_and_timed_out_stops_keep_collected_failures_until_resources_end() {
        use std::{future::Future, task::Poll};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(10), async {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let mut application = start(&server, &name, 0, 8, &supplier).await;
                application.settings.stop = Duration::from_millis(30);
                let blocker = connections(&server, &name, "postgres").recover().await.expect("fixture blocker");
                blocker.client().batch_execute("BEGIN; SELECT pg_advisory_xact_lock(1314210636, 1)")
                    .await.expect("hold accepted work");
                let service = application.authentication().clone();
                let worker = tokio::spawn(async move {
                    service.verify(input(OPERATION, "13800000000", "fictional-token")).await
                });
                wait_for_lock(blocker.client()).await;
                // Represent an HTTP failure already collected before draining accepted work.
                application.failures.push(Arc::new(process::Error::Http(process::transport::Error::Transport(
                    std::io::ErrorKind::BrokenPipe.into(),
                ))));
                let mut stopping = Box::pin(application.stop());
                std::future::poll_fn(|context| {
                    assert!(stopping.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                }).await;
                drop(stopping);
                let error = application.stop().await.expect_err("accepted work remains blocked");
                let process::Error::Timeout { failures, .. } = &error else {
                    panic!("expected the stop deadline");
                };
                assert!(matches!(failures.as_slice(), [failure]
                    if matches!(failure.as_ref(), process::Error::Http(process::transport::Error::Transport(cause))
                        if cause.kind() == std::io::ErrorKind::BrokenPipe)));
                assert!(instance_held(&server, &name).await);
                blocker.client().batch_execute("COMMIT").await.expect("release accepted work");
                application.settings.stop = Duration::from_secs(5);
                let final_error = application.stop().await.expect_err("collected failure survives the earlier report");
                assert!(matches!(final_error, process::Error::Shutdown(ref failures)
                    if matches!(failures.as_slice(), [failure]
                        if matches!(failure.as_ref(), process::Error::Http(process::transport::Error::Transport(cause))
                            if cause.kind() == std::io::ErrorKind::BrokenPipe))));
                assert!(matches!(application.phase, process::Phase::Stopped));
                worker.await.expect("worker task").expect("accepted work completed");
                application.stop().await.expect("completed shutdown is not repeated");
                assert!(instance_released(&server, &name).await);
                drop(application);
                assert!(instance_released(&server, &name).await);
                blocker.terminate().await.expect("fixture blocker ended");
            }).await.expect("collected failure test budget");
        });
    }

    #[test]
    fn http_stop_timeout_preserves_accepted_work_and_the_instance_lock() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let name = database(&server).await;
            let reader = connections(&server, &name, "clerk").recover().await.expect("fixture time");
            let now: chrono::DateTime<chrono::Utc> = reader.client().query_one("SELECT clock_timestamp()", &[]).await.expect("time").get(0);
            reader.terminate().await.expect("reader ends");
            let supplier = Supplier::responding(serde_json::json!({"success":true,"challenge_ts":now,"hostname":"example.com","action":"send_authentication_code","cdata":OPERATION}).to_string()).await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(8, domain::identity::message::Limits::default(), session_settings(), domain::registration::Limits::default(), avatar_settings()).expect("settings"), process::Settings::new(Duration::from_millis(20)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("start");
            let (base, stop, task) = running(application).await;
            let caller = tokio::spawn(http_client().post(format!("{base}/api/v1/codes")).header("operation-id", OPERATION).json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"})).send());
            let sent = timeout(Duration::from_secs(4), sms.request()).await.expect("accepted send");
            stop.send(()).expect("stop");
            let (mut application, result) = timeout(Duration::from_secs(1), task).await.expect("bounded stop").expect("HTTP task");
            let error = result.expect_err("unfinished stop");
            assert!(matches!(error, process::RunError::Application(process::Error::Timeout { .. })));
            assert!(error.incomplete());
            assert!(instance_held(&server, &name).await);
            accept_sms(sent);
            assert_eq!(timeout(Duration::from_secs(3), caller).await.expect("original completion").expect("caller").expect("HTTP response").status(), 200);
            // Only the fixture waits for completion after this failure; production exits.
            application.settings.stop = Duration::from_secs(5);
            application.stop().await.expect("end fixture resources");
            drop(application);
        });
    }

    #[test]
    fn isolated_http_process() {
        let Ok(name) = std::env::var("NUCLEUS_HTTP_TEST_DATABASE") else {
            return;
        };
        assert!(name.starts_with("test_"));
        crate::diagnostics::init().expect("online diagnostics");
        let value = |name| std::env::var(name).expect("isolated fixture setting");
        let port = value("NUCLEUS_HTTP_TEST_PORT")
            .parse()
            .expect("fixture port");
        let authority =
            std::fs::read(value("NUCLEUS_HTTP_TEST_AUTHORITY")).expect("fixture certificate");
        let connections = Connections::new(
            database::Settings::new(
                "127.0.0.1".into(),
                port,
                name,
                "clerk".into(),
                2,
                database::settings::Budgets::default(),
            )
            .expect("settings"),
            Password::new(fixture::PASSWORD.into()).expect("fictional password"),
            database::assembly::roots(&authority).expect("fixture roots"),
        )
        .expect("connections");
        let supplier = client::Client::for_test(
            value("NUCLEUS_HTTP_TEST_CHALLENGE_PORT")
                .parse()
                .expect("fixture port"),
            challenge::settings::Settings::new(
                challenge::settings::AllowedHostnames::new(NonEmpty::new("example.com".into()))
                    .expect("hostnames"),
            ),
            "fictional-secret".parse().expect("secret"),
        )
        .expect("fixture client");
        let address: std::net::SocketAddr = value("NUCLEUS_HTTP_TEST_MESSAGE_ADDRESS")
            .parse()
            .expect("fixture address");
        assert!(address.ip().is_loopback());
        let authority = std::fs::read(value("NUCLEUS_HTTP_TEST_MESSAGE_AUTHORITY"))
            .expect("fixture certificate");
        let sms = message_at(address, &authority);
        let ready: std::net::SocketAddr = value("NUCLEUS_HTTP_TEST_READY")
            .parse()
            .expect("fixture readiness address");
        assert!(ready.ip().is_loopback());
        let listen: std::net::SocketAddr = std::env::var("NUCLEUS_HTTP_TEST_LISTEN")
            .unwrap_or_else(|_| "127.0.0.1:0".into())
            .parse()
            .expect("fixture listener");
        assert!(listen.ip().is_loopback());
        let stop = Duration::from_millis(
            std::env::var("NUCLEUS_HTTP_TEST_STOP_MILLIS")
                .unwrap_or_else(|_| "5000".into())
                .parse()
                .expect("fixture stop budget"),
        );
        Runtime::new().expect("runtime").block_on(async {
            let starting = Application::start(
                connections,
                keys(0),
                Settings::new(
                    8,
                    domain::identity::message::Limits::default(),
                    session_settings(),
                    domain::registration::Limits::default(),
                    avatar_settings(),
                )
                .expect("C1 settings"),
                process::Settings::new(stop).expect("stop budget"),
                supplier,
                sms,
            );
            let settings = crate::identity::Settings::new(
                "https://example.com".into(),
                "fixture-site-key".into(),
            )
            .expect("origin settings");
            process::supervise(
                starting,
                stop,
                listen,
                crate::source::Settings::new(vec!["127.0.0.1/32".parse().expect("proxy")])
                    .expect("source settings"),
                settings,
                |address| {
                    use std::io::Write;
                    let mut ready =
                        std::net::TcpStream::connect(ready).expect("fixture readiness connection");
                    ready
                        .write_all(address.to_string().as_bytes())
                        .expect("fixture readiness message");
                    Ok(())
                },
            )
            .await
            .expect("signal drains the application");
        });
    }

    async fn startup_stop(server: &fixture::Server, signal: &str, released: bool) {
        timeout(Duration::from_secs(20), async {
            let directory = tempfile::tempdir().expect("startup stop fixtures");
            let name = database(server).await;
            let supplier = Supplier::new().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let certificate = directory.path().join("supplier.der");
            std::fs::write(&certificate, sms.authority()).expect("supplier certificate");
            let proxy = fixture::database_proxy::Proxy::start(server).await;
            proxy.arm(fixture::database_proxy::Fault::StalledHistory);
            let ready = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("readiness observer");
            // A successful stop must never try to bind this occupied address.
            let occupied = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("occupied HTTP address");
            let stop = Duration::from_millis(if released { 3000 } else { 1000 });
            let mut child =
                tokio::process::Command::new(std::env::current_exe().expect("test binary"))
                    .args([
                        "--exact",
                        "process::tests::isolated_http_process",
                        "--nocapture",
                    ])
                    .env("NUCLEUS_HTTP_TEST_DATABASE", &name)
                    .env("NUCLEUS_HTTP_TEST_PORT", proxy.port().to_string())
                    .env("NUCLEUS_HTTP_TEST_AUTHORITY", server.authority())
                    .env(
                        "NUCLEUS_HTTP_TEST_CHALLENGE_PORT",
                        supplier.port.to_string(),
                    )
                    .env(
                        "NUCLEUS_HTTP_TEST_MESSAGE_ADDRESS",
                        sms.address().to_string(),
                    )
                    .env("NUCLEUS_HTTP_TEST_MESSAGE_AUTHORITY", &certificate)
                    .env(
                        "NUCLEUS_HTTP_TEST_READY",
                        ready.local_addr().expect("observer address").to_string(),
                    )
                    .env(
                        "NUCLEUS_HTTP_TEST_LISTEN",
                        occupied.local_addr().expect("occupied address").to_string(),
                    )
                    .env(
                        "NUCLEUS_HTTP_TEST_STOP_MILLIS",
                        stop.as_millis().to_string(),
                    )
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .expect("startup child");
            let mut lines = BufReader::new(child.stderr.take().expect("child diagnostics")).lines();
            timeout(Duration::from_secs(5), proxy.intercepted())
                .await
                .expect("startup history fetch held");
            let admin = connections(server, &name, "postgres")
                .recover()
                .await
                .expect("independent observer");
            let rows = admin
                .client()
                .query(
                    "SELECT pid FROM pg_catalog.pg_stat_activity
                     WHERE datname = $1 AND usename = 'clerk' ORDER BY pid",
                    &[&name],
                )
                .await
                .expect("startup backends");
            let backends: Vec<i32> = rows.iter().map(|row| row.get(0)).collect();
            assert_eq!(
                backends.len(),
                2,
                "startup barrier and trusted clock sessions"
            );
            let held: bool = admin
                .client()
                .query_one(
                    "SELECT EXISTS (
                         SELECT FROM pg_catalog.pg_locks
                         WHERE locktype = 'advisory' AND classid = 1314210636
                           AND objid = 1 AND objsubid = 2 AND mode = 'ExclusiveLock'
                           AND granted AND pid = ANY($1)
                     )",
                    &[&backends],
                )
                .await
                .expect("independent startup barrier observation")
                .get(0);
            assert!(held);
            assert!(instance_held(server, &name).await);
            let process_id = child.id().expect("owned child PID");
            assert!(
                tokio::process::Command::new("kill")
                    .args([signal, &process_id.to_string()])
                    .status()
                    .await
                    .expect("signal owned child")
                    .success()
            );
            let mut events: Vec<serde_json::Value> = Vec::new();
            timeout(Duration::from_secs(2), async {
                loop {
                    let line = lines
                        .next_line()
                        .await
                        .expect("diagnostic read")
                        .expect("stop observed before exit");
                    let event: serde_json::Value = serde_json::from_str(&line).expect("JSON event");
                    let stopping =
                        event["fields"]["stage"] == "start_stop" && event["level"] == "INFO";
                    events.push(event);
                    if stopping {
                        break;
                    }
                }
            })
            .await
            .expect("startup signal handled");
            assert!(
                child.try_wait().expect("child state").is_none(),
                "held verification remains owned after the signal"
            );
            assert!(instance_held(server, &name).await);
            if released {
                proxy.release();
            }
            let status = timeout(stop + Duration::from_secs(1), async {
                tokio::select! {
                    biased;
                    _ = ready.accept() => panic!("startup stop must never announce readiness"),
                    _ = sms.request() => panic!("startup stop must never send an SMS"),
                    result = child.wait() => result.expect("confirmed child exit"),
                }
            })
            .await
            .expect("one bounded startup stop");
            assert_eq!(status.code(), Some(if released { 0 } else { 1 }));
            while let Some(line) = lines.next_line().await.expect("remaining diagnostics") {
                events.push(serde_json::from_str(&line).expect("JSON event"));
            }
            assert!(
                events
                    .iter()
                    .all(|event| event["fields"]["stage"] != "ready")
            );
            let errors: Vec<_> = events
                .iter()
                .filter(|event| event["level"] == "ERROR")
                .collect();
            if released {
                assert!(errors.is_empty());
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event["fields"]["stage"] == "stopped")
                        .count(),
                    1
                );
            } else {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0]["fields"]["stage"], "start_stop");
                assert_eq!(errors[0]["fields"]["kind"], "timeout");
                assert!(
                    events
                        .iter()
                        .all(|event| event["fields"]["stage"] != "stopped")
                );
                // The proxy can outlive the exited client; let it observe the closed connection.
                proxy.release();
            }
            assert!(
                instance_released(server, &name).await,
                "lock reusable after confirmed exit"
            );
            timeout(Duration::from_secs(3), async {
                loop {
                    let remaining: i64 = admin
                        .client()
                        .query_one(
                            "SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE pid = ANY($1)",
                            &[&backends],
                        )
                        .await
                        .expect("original backend termination")
                        .get(0);
                    if remaining == 0 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("old database sessions ended");
            proxy.stop().await;
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 0);
            assert!(
                timeout(Duration::from_millis(50), ready.accept())
                    .await
                    .is_err(),
                "no queued readiness notification"
            );
            assert!(
                timeout(Duration::from_millis(50), sms.request())
                    .await
                    .is_err(),
                "no queued supplier call"
            );
            sms.stop().await;
            admin.terminate().await.expect("observer ended");
        })
        .await
        .expect("startup stop scenario budget");
    }

    #[test]
    fn startup_stop_waits_for_verification_without_serving() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for signal in ["-TERM", "-INT"] {
                startup_stop(&server, signal, true).await;
            }
        });
    }

    #[test]
    fn startup_stop_exits_unsuccessfully_when_verification_exceeds_the_budget() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for signal in ["-TERM", "-INT"] {
                startup_stop(&server, signal, false).await;
            }
        });
    }

    #[test]
    fn http_recovers_original_message_and_registration_after_confirmed_process_exit() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let directory = tempfile::tempdir().expect("process fixtures");
            let name = database(&server).await;
            let reader = connections(&server,&name,"clerk").recover().await.expect("time fixture");
            let now: chrono::DateTime<chrono::Utc> = reader.client().query_one("SELECT clock_timestamp()",&[]).await.expect("time").get(0);
            reader.terminate().await.expect("reader ends");
            let supplier = Supplier::responding(serde_json::json!({"success":true,"challenge_ts":now,"hostname":"example.com","action":"send_authentication_code","cdata":OPERATION}).to_string()).await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let certificate = directory.path().join("supplier.der");
            std::fs::write(&certificate,sms.authority()).expect("fixture TLS authority");
            let client = http_client();
            let mut option = None;
            let mut registration = None;
            for attempt in 0..2 {
                let ready = TcpListener::bind("127.0.0.1:0").await.expect("readiness IPC");
                let mut command = tokio::process::Command::new(std::env::current_exe().expect("test executable"));
                command.args(["--exact","process::tests::isolated_http_process","--nocapture"])
                    .env("NUCLEUS_HTTP_TEST_DATABASE",&name)
                    .env("NUCLEUS_HTTP_TEST_PORT",server.port().to_string())
                    .env("NUCLEUS_HTTP_TEST_AUTHORITY",server.authority())
                    .env("NUCLEUS_HTTP_TEST_CHALLENGE_PORT",supplier.port.to_string())
                    .env("NUCLEUS_HTTP_TEST_MESSAGE_ADDRESS",sms.address().to_string())
                    .env("NUCLEUS_HTTP_TEST_MESSAGE_AUTHORITY",&certificate)
                    .env("NUCLEUS_HTTP_TEST_READY",ready.local_addr().expect("IPC address").to_string())
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
                let mut child = command.spawn().expect("isolated HTTP child");
                let (mut socket,_) = timeout(Duration::from_secs(10),ready.accept()).await.expect("startup budget").expect("ready");
                let mut address = String::new();
                timeout(Duration::from_secs(1),socket.read_to_string(&mut address)).await.expect("readiness budget").expect("address");
                let request = client.post(format!("http://{address}/api/v1/codes")).header("operation-id",OPERATION).json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"}));
                let caller = tokio::spawn(request.send());
                let code = if attempt == 0 {
                    let sent = timeout(Duration::from_secs(4),sms.request()).await.expect("one real fixture call");
                    let text = std::str::from_utf8(&sent.bytes).expect("SMS wire");
                    let target = text.lines().next().expect("line").split(' ').nth(1).expect("target");
                    let url = url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("URL");
                    let template = url.query_pairs().find(|(name,_)|name=="TemplateParam").expect("template").1.into_owned();
                    let code = serde_json::from_str::<serde_json::Value>(&template).expect("template JSON")["code"].as_str().expect("code").to_owned();
                    accept_sms(sent); Some(code)
                } else { None };
                let response = caller.await.expect("caller task").expect("HTTP response");
                assert_eq!(response.status(),200);
                let receipt: serde_json::Value = response.json().await.expect("message response");
                if let Some(original) = &option { assert_eq!(&receipt,original); } else { option = Some(receipt.clone()); }
                if let Some(code) = code {
                    let response = client.post(format!("http://{address}/api/v1/sessions")).header("operation-id",OPERATION).json(&serde_json::json!({"secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","credential":{"type":"message","id":receipt["message"]["id"],"mobile":"13800000000","code":code}})).send().await.expect("R1 in first child");
                    assert_eq!(response.status(),200);
                    let response: serde_json::Value = response.json().await.expect("registration credential");
                    let body = serde_json::json!({"id":response["registration"]["id"],"token":response["registration"]["token"],"name":"Fixture Operator","idNumber":"00000020000229007X"});
                    let response = client.post(format!("http://{address}/api/v1/registrations")).header("operation-id",OPERATION).json(&body).send().await.expect("R2 in first child");
                    assert_eq!(response.status(),201);
                    let cookie = response.headers().get("set-cookie").expect("original cookie").clone();
                    let result: serde_json::Value = response.json().await.expect("original session");
                    let observer = connections(&server,&name,"clerk").recover().await.expect("independent commit observer");
                    let row = observer.client().query_one("SELECT count(*) AS completions, bool_and(attempt.id=receipt.id AND attempt.operation_id=receipt.operation_id AND receipt.account_id=session.account_id AND session.created_at=attempt.accepted_at AND authentication.status='registrable' AND authentication.operation_id=receipt.operation_id) AS paired FROM identity.attempts attempt JOIN identity.receipts receipt ON receipt.id=attempt.id JOIN identity.sessions session ON session.receipt_id=receipt.id JOIN identity.authentications authentication ON authentication.id=session.authentication_id", &[]).await.expect("all three families share a UUID without conflating their originals");
                    assert_eq!(row.get::<_,i64>("completions"),1); assert!(row.get::<_,bool>("paired")); observer.terminate().await.expect("observer ended");
                    registration=Some((body,result,cookie));
                } else {
                    let (body,result,cookie)=registration.as_ref().expect("first child committed registration");
                    let response=client.post(format!("http://{address}/api/v1/registrations")).header("operation-id",OPERATION).json(body).send().await.expect("R2 after child.wait");
                    assert_eq!(response.status(),200); assert_eq!(response.headers().get("set-cookie"),Some(cookie));
                    assert_eq!(&response.json::<serde_json::Value>().await.expect("original session"),result);
                    let observer=connections(&server,&name,"clerk").recover().await.expect("independent restart observer");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.attempts",&[]).await.expect("one admission after restart").get::<_,i64>(0),1);
                    observer.terminate().await.expect("observer ended");
                }
                let process_id = child.id().expect("owned child PID");
                assert!(tokio::process::Command::new("kill").args(["-TERM",&process_id.to_string()]).status().await.expect("signal owned fixture").success());
                assert!(timeout(Duration::from_secs(7),child.wait()).await.expect("confirmed process exit").expect("reap owned process").success());
                assert!(instance_released(&server, &name).await, "old process released lock");
            }
            let mut preparation=Application::start(connections(&server,&name,"clerk"),keys(0),Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("authenticated original history");
            let mut admin=connections(&server,&name,"postgres").recover().await.expect("isolated retained R2 history");
            preparation.registration().age_for_test(admin.client_mut(),chrono::TimeDelta::hours(25)).await.expect("consistent historical graph");
            preparation.stop().await.expect("fixture application drained");drop(preparation);
            let application=Application::start(connections(&server,&name,"clerk"),keys(0),Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"),process::Settings::new(Duration::from_millis(30)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("historical startup");
            let operation:Uuid=admin.client().query_one("SELECT operation_id FROM identity.receipts",&[]).await.expect("original permanent R2 operation").get(0);
            let lock=registration_cleanup_lock(operation);
            admin.client().batch_execute("BEGIN").await.expect("fixture lock transaction");
            admin.client().query_one("SELECT pg_catalog.pg_advisory_xact_lock($1)",&[&lock]).await.expect("hold only the original R2 cleanup operation");
            let pid:i32=admin.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("blocker").get(0);
            let (_,stop,task)=running(application).await;
            let observer=connections(&server,&name,"clerk").recover().await.expect("independent cleanup waiter observer");
            timeout(Duration::from_secs(5),blocked_registration_cleanup(observer.client(),pid,lock)).await.expect("background cleanup reaches the original R2 operation");
            stop.send(()).expect("stop during R2 cleanup");
            let (mut application,result)=timeout(Duration::from_secs(2),task).await.expect("bounded stop").expect("process task");
            assert!(matches!(result,Err(process::RunError::Application(process::Error::Timeout{..}))));
            assert!(instance_held(&server, &name).await);
            admin.client().batch_execute("COMMIT").await.expect("release owned cleanup");
            application.settings.stop=Duration::from_secs(5); application.stop().await.expect("current R2 cleanup finishes before resources end");
            let row=admin.client().query_one("SELECT (SELECT count(*) FROM identity.attempts) AS attempts,(SELECT count(*) FROM identity.handoffs) AS handoffs,(SELECT count(*) FROM identity.sessions) AS sessions,(SELECT count(*) FROM identity.receipts) AS receipts",&[]).await.expect("independent final cleanup effects");
            assert_eq!(row.get::<_,i64>("attempts"),0); assert_eq!(row.get::<_,i64>("handoffs"),0); assert_eq!(row.get::<_,i64>("sessions"),1); assert_eq!(row.get::<_,i64>("receipts"),1);
            drop(application); observer.terminate().await.expect("observer ended"); admin.terminate().await.expect("fixture ended");
            assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst),1);
            assert!(timeout(Duration::from_millis(50),sms.request()).await.is_err(),"a fresh process cannot resend");
            sms.stop().await;
        });
    }

    fn registration_cleanup_lock(operation: Uuid) -> i64 {
        let input = platform::cryptography::Encoded::fields(&[
            b"registration/operation",
            operation.as_bytes(),
        ])
        .expect("fixed original-operation lock encoding");
        let digest = platform::cryptography::sha256::Digest::new(&input);
        i64::from_be_bytes(digest.as_ref()[..8].try_into().expect("lock key width"))
    }

    async fn blocked_registration_cleanup(
        client: &tokio_postgres::Client,
        blocker: i32,
        lock: i64,
    ) -> i32 {
        let bytes = lock.to_be_bytes();
        let class = i64::from(u32::from_be_bytes(
            bytes[..4].try_into().expect("class width"),
        ));
        let object = i64::from(u32::from_be_bytes(
            bytes[4..].try_into().expect("object width"),
        ));
        loop {
            if let Some(row) = client.query_opt(
                "SELECT waiter.pid FROM pg_catalog.pg_locks waiter JOIN pg_catalog.pg_stat_activity activity USING (pid) WHERE activity.datname=current_database() AND activity.usename='clerk' AND waiter.locktype='advisory' AND waiter.mode='ExclusiveLock' AND NOT waiter.granted AND waiter.classid::bigint=$2 AND waiter.objid::bigint=$3 AND waiter.objsubid=1 AND $1=ANY(pg_catalog.pg_blocking_pids(waiter.pid))",
                &[&blocker, &class, &object],
            ).await.expect("the exact R2 operation waits on the fixture backend") {
                return row.get(0);
            }
            tokio::task::yield_now().await;
        }
    }

    #[test]
    fn session_retention_keeps_handoff_deletion_known_during_process_stop() {
        use fixture::database_proxy::{Fault, Proxy};

        if std::env::var_os("NUCLEUS_TEST_SESSION_RETENTION").is_none() {
            Runtime::new().expect("parent runtime").block_on(async {
                let expected = tempfile::NamedTempFile::new().expect("private fixture expectations");
                let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact","process::tests::session_retention_keeps_handoff_deletion_known_during_process_stop","--nocapture"])
                    .env("NUCLEUS_TEST_SESSION_RETENTION","1").env("NUCLEUS_TEST_RETENTION_EXPECTATIONS",expected.path())
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true)
                    .spawn().expect("isolated diagnostics process");
                let output = timeout(Duration::from_secs(35),child.wait_with_output()).await.expect("child budget").expect("child output");
                let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
                assert!(output.status.success(),"{text}");
                let expected: serde_json::Value = serde_json::from_slice(&std::fs::read(expected.path()).expect("private fixture readback")).expect("expected operation and actual token");
                let events: Vec<serde_json::Value> = text.lines().map(|line|serde_json::from_str(line).expect("JSON event")).collect();
                let failures: Vec<_> = events.iter().filter(|event|event["level"]=="ERROR").collect();
                assert_eq!(failures.len(),1,"one final session cleanup diagnostic: {text}");
                let event = failures[0];
                assert_eq!(event["fields"]["stage"],"session_retention");
                assert_eq!(event["fields"]["operation_id"],expected["operation"]);
                let error = event["fields"]["error"].as_str().expect("retained causes");
                assert!(error.starts_with("SessionDeleted { cause: SessionReadback {"),"{event}");
                assert!(error.contains("Query(connection closed)"),"original deletion error retained");
                assert!(error.contains("commit: Err(CommitTimeout(Elapsed"),"{event}");
                assert!(error.contains("termination: Ok(())"),"{event}");
                assert!(!error.contains("Unknown"));
                for secret in [expected["token"].as_str().expect("actual token"),"13900000000","Fixture Operator","000000200002290010","__Host-nucleus-session",fixture::PASSWORD,"SELECT ","DELETE "] {
                    assert!(!events.iter().any(|event|contains_text(event,secret)),"private material in diagnostics");
                }
            });
            return;
        }
        crate::diagnostics::init().expect("actual online diagnostics");
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30),async {
                let name = database(&server).await;
                let supplier = Supplier::echoing().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let application = Application::start(connections(&server,&name,"clerk"),keys(0),Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop settings"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("initial application");
                let (base,stop,task) = running(application).await;
                let (session_id,cookie) = issued_session_for(&base,&mut sms,"13900000000").await;
                stop.send(()).expect("stop original issuance"); let (application,result)=task.await.expect("original task"); result.expect("original drain"); let issuance=application.registration().clone(); drop(application);
                let mut admin = connections(&server,&name,"postgres").recover().await.expect("isolated history owner");
                issuance.age_for_test(admin.client_mut(),chrono::TimeDelta::minutes(11)).await.expect("consistent sealed history with expired handoff and valid session");
                let proxy = Proxy::start(&server).await;
                let application = Application::start(proxy_connections(&name,&proxy),keys(0),Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"),process::Settings::new(Duration::from_millis(30)).expect("short stop budget"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("healthy historical startup");
                admin.client().batch_execute("BEGIN; SELECT FROM identity.accounts FOR UPDATE").await.expect("hold R1 cleanup account");
                let blocker: i32 = admin.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("account blocker").get(0);
                let observer = connections(&server,&name,"clerk").recover().await.expect("independent observer");
                let original = observer.client().query_one("SELECT authentication.id,authentication.operation_id,session.expires_at FROM identity.authentications authentication JOIN identity.sessions session ON session.authentication_id=authentication.id WHERE session.id=$1::text::uuid AND session.receipt_id IS NULL", &[&session_id]).await.expect("original R1 authentication");
                let authentication_id: Uuid = original.get("id");
                let operation: Uuid = original.get("operation_id");
                let expires_at: chrono::DateTime<chrono::Utc> = original.get("expires_at");
                // Parent-owned 0600 material is separate from captured application diagnostics.
                std::fs::write(std::env::var_os("NUCLEUS_TEST_RETENTION_EXPECTATIONS").expect("private fixture path"),serde_json::to_vec(&serde_json::json!({"operation":operation,"token":cookie.split_once('=').expect("cookie pair").1})).expect("expectations JSON")).expect("private expectations");
                let (_,stop,task) = running(application).await;
                let original_pid = timeout(Duration::from_secs(3),async { loop {
                    if let Some(row)=observer.client().query_opt("SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND query='SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE' AND $1=ANY(pg_catalog.pg_blocking_pids(pid))", &[&blocker]).await.expect("actual R1 account waiter") { break row.get::<_,i32>(0); }
                    tokio::task::yield_now().await;
                }}).await.expect("R1 retention reaches account protection");
                proxy.arm(Fault::DelayedCommit); admin.client().batch_execute("COMMIT").await.expect("release deletion");
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("R1 delete COMMIT held");
                let recovery_pid = timeout(Duration::from_secs(3),async { loop {
                    if let Some(row)=observer.client().query_opt("SELECT pid FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND $1=ANY(pg_catalog.pg_blocking_pids(pid))", &[&original_pid]).await.expect("original R1 backend blocks recovery") { break row.get::<_,i32>(0); }
                    tokio::task::yield_now().await;
                }}).await.expect("readback waits for original operation");
                proxy.arm(Fault::StalledReadCommit); proxy.release();
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("actual readback COMMIT held");
                let effects = "SELECT NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$1::text::uuid) AND EXISTS(SELECT 1 FROM identity.sessions WHERE id=$1::text::uuid AND authentication_id=$2 AND receipt_id IS NULL AND expires_at=$4 AND expires_at>clock_timestamp()) AND EXISTS(SELECT 1 FROM identity.authentications WHERE id=$2 AND operation_id=$3 AND status='authenticated')";
                assert!(observer.client().query_one(effects,&[&session_id,&authentication_id,&operation,&expires_at]).await.expect("independent deletion effects before stop or later cleanup").get::<_,bool>(0));
                stop.send(()).expect("stop during retained readback work");
                let (mut application,result)=timeout(Duration::from_secs(3),task).await.expect("initial stop bounded").expect("process task");
                assert!(matches!(result,Err(process::RunError::Application(process::Error::Timeout{..}))));
                assert!(instance_held(&server, &name).await,"stop timeout preserves ownership");
                application.settings.stop=Duration::from_secs(15); application.stop().await.expect("original background readback drains before resources close"); drop(application);
                proxy.release();
                timeout(Duration::from_secs(3),async { loop {
                    if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1))", &[&vec![original_pid,recovery_pid]]).await.expect("actual backend termination").get::<_,bool>(0) { break; }
                    tokio::task::yield_now().await;
                }}).await.expect("both original and recovery backends ended");
                assert!(observer.client().query_one(effects,&[&session_id,&authentication_id,&operation,&expires_at]).await.expect("effects remain before restart").get::<_,bool>(0));
                let application = Application::start(connections(&server,&name,"clerk"),keys(0),Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop settings"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("restart without handoff");
                let (base,stop,task)=running(application).await;
                let response=http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id",OPERATION).header("session-id",&session_id).header("cookie",cookie).send().await.expect("original session remains usable"); assert_eq!(response.status(),200);
                stop.send(()).expect("stop later round"); let (application,result)=task.await.expect("later process task"); result.expect("later drain"); drop(application);
                assert!(observer.client().query_one(effects,&[&session_id,&authentication_id,&operation,&expires_at]).await.expect("later cleanup does not replace the session").get::<_,bool>(0));
                observer.terminate().await.expect("observer ended"); admin.terminate().await.expect("owner ended");
                assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst),1);
                assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no extra supplier call through restart and drain");
                sms.stop().await; proxy.stop().await;
            }).await.expect("actual retention process budget");
        });
    }

    #[test]
    fn registration_retention_keeps_deleted_outcomes_and_drains_readback_cleanup() {
        use fixture::database_proxy::{Fault, Proxy};

        if std::env::var_os("NUCLEUS_TEST_REGISTRATION_RETENTION").is_none() {
            Runtime::new().expect("parent runtime").block_on(async {
                let expected = tempfile::NamedTempFile::new().expect("private fixture expectations");
                let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", "process::tests::registration_retention_keeps_deleted_outcomes_and_drains_readback_cleanup", "--nocapture"])
                    .env("NUCLEUS_TEST_REGISTRATION_RETENTION", "1")
                    .env("NUCLEUS_TEST_RETENTION_EXPECTATIONS", expected.path())
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true)
                    .spawn().expect("isolated diagnostics process");
                let output = timeout(Duration::from_secs(45), child.wait_with_output()).await.expect("child budget").expect("child output");
                let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
                assert!(output.status.success(), "{text}");
                let expected: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(expected.path()).expect("private fixture readback")).expect("expected operations and cookies");
                assert_eq!(expected.len(), 2);
                let events: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).expect("JSON event")).collect();
                let failures: Vec<_> = events.iter().filter(|event| event["level"] == "ERROR").collect();
                assert_eq!(failures.len(), 2, "one final diagnostic per actual cleanup: {text}");
                for ((expected, event), stalled) in expected.iter().zip(&failures).zip([true, false]) {
                    assert_eq!(event["fields"]["stage"], "registration_retention");
                    assert_eq!(event["fields"]["operation_id"], expected["operation"], "diagnostic belongs to the independently queried Receipt");
                    let token = expected["token"].as_str().expect("actual cookie value");
                    assert!(!events.iter().any(|event| contains_text(event, token)), "actual generated session token is absent from diagnostics");
                    let error = event["fields"]["error"].as_str().expect("complete safe cleanup diagnostic");
                    assert!(error.starts_with("Deleted { cause: Readback {"), "{event}");
                    assert!(error.contains("Query(connection closed)"), "original deletion failure is retained: {event}");
                    if stalled { assert!(error.contains("commit: Err(CommitTimeout(Elapsed"), "{event}"); }
                    else { assert!(error.contains("commit: Ok(())")); assert!(error.contains("termination: Err(database driver connection failed)"), "{event}"); }
                    assert!(!error.contains("Unknown"));
                }
                assert_ne!(failures[0]["fields"]["operation_id"], failures[1]["fields"]["operation_id"]);
                for secret in ["fictional-sensitive-termination-detail", "Fixture Registered", "Fixture Operator", "13800000000", "13900000000", "__Host-nucleus-session", "00000020000229007X", "000000200002290010", fixture::PASSWORD, "SELECT ", "DELETE "] {
                    assert!(!events.iter().any(|event| contains_text(event, secret)), "private material in diagnostics: {secret}");
                }
            });
            return;
        }
        crate::diagnostics::init().expect("actual online diagnostics");
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("actual driver observation runtime").block_on(async {
            let mut expected = Vec::new();
            for fault in [Fault::StalledReadCommit, Fault::ReadCommitTermination] {
                timeout(Duration::from_secs(30), async {
                    let name = database(&server).await;
                    let supplier = Supplier::echoing().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let application = Application::start(connections(&server,&name,"clerk"), keys(0), Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"), process::Settings::new(Duration::from_secs(5)).expect("stop settings"), supplier.client(), message_at(sms.address(),sms.authority())).await.expect("initial online application");
                    let (base,stop,task) = running(application).await;
                    let (session_id,cookie) = issued_session_for(&base,&mut sms,"13800000000").await;
                    let response = http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id",OPERATION).header("session-id",&session_id).header("cookie",&cookie).send().await.expect("complete original credentials before cleanup");
                    assert_eq!(response.status(),200);
                    assert!(!response.headers().contains_key("set-cookie"));
                    stop.send(()).expect("stop original issuance");
                    let (application,result) = task.await.expect("original process task"); result.expect("original work drained"); let issuance=application.registration().clone(); drop(application);
                    let mut admin = connections(&server,&name,"postgres").recover().await.expect("isolated history owner");
                    issuance.age_for_test(admin.client_mut(),chrono::TimeDelta::hours(49)).await.expect("consistent isolated historical graph");
                    let proxy = Proxy::start(&server).await;
                    let stalled = matches!(fault,Fault::StalledReadCommit);
                    // Only the stalled round asserts a stop timeout. The other stop drains cleanup that is still
                    // running after the injected FATAL, so a short budget would race scheduling instead of testing it.
                    let budget = if stalled { Duration::from_millis(30) } else { Duration::from_secs(5) };
                    let application = Application::start(proxy_connections(&name,&proxy), keys(0), Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"), process::Settings::new(budget).expect("round stop budget"), supplier.client(), message_at(sms.address(),sms.authority())).await.expect("healthy historical startup");
                    let operation:Uuid=admin.client().query_one("SELECT operation_id FROM identity.receipts",&[]).await.expect("original R2 operation").get(0);
                    let lock=registration_cleanup_lock(operation);
                    admin.client().batch_execute("BEGIN").await.expect("fixture lock transaction");
                    admin.client().query_one("SELECT pg_catalog.pg_advisory_xact_lock($1)",&[&lock]).await.expect("hold precisely the R2 cleanup operation");
                    let blocker: i32 = admin.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("account blocker").get(0);
                    let observer = connections(&server,&name,"clerk").recover().await.expect("independent observer");
                    let original = observer.client().query_one("SELECT id,operation_id FROM identity.receipts",&[]).await.expect("permanent completion");
                    let receipt_id: Uuid = original.get("id");
                    let operation: Uuid = original.get("operation_id");
                    expected.push(serde_json::json!({"operation":operation,"token":cookie.split_once('=').expect("cookie pair").1}));
                    let (_,stop,task) = running(application).await;
                    let original_pid = timeout(Duration::from_secs(3),blocked_registration_cleanup(observer.client(),blocker,lock)).await.expect("R2 cleanup started after C1 and R1 phases");
                    proxy.arm(Fault::DelayedCommit);
                    admin.client().batch_execute("COMMIT").await.expect("release R2 deletion");
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("actual R2 deletion commit held");
                    let recovery_pid = timeout(Duration::from_secs(3),async { loop {
                        if let Some(row) = observer.client().query_opt("SELECT pid FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND $1=ANY(pg_catalog.pg_blocking_pids(pid))", &[&original_pid]).await.expect("same original cleanup backend blocks recovery") { break row.get::<_,i32>(0); }
                        tokio::task::yield_now().await;
                    }}).await.expect("recovery crosses actual original operation lock");
                    proxy.arm(fault); proxy.release();
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("actual cleanup readback fault");
                    let deleted: bool = observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.attempts WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.sessions WHERE id=$2::text::uuid) AND NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$2::text::uuid) AND EXISTS(SELECT 1 FROM identity.receipts WHERE id=$1 AND operation_id=$3)", &[&receipt_id,&session_id,&operation]).await.expect("independent effects before stop and next cleanup").get(0);
                    assert!(deleted);
                    stop.send(()).expect("stop while readback cleanup remains owned");
                    let (mut application,result) = timeout(Duration::from_secs(3),task).await.expect("initial stop bounded").expect("process task");
                    if stalled {
                        assert!(matches!(result,Err(process::RunError::Application(process::Error::Timeout{..}))));
                        assert!(instance_held(&server, &name).await, "stop timeout retains instance ownership");
                        application.settings.stop = Duration::from_secs(15);
                        application.stop().await.expect("same background work completes before resources close");
                    } else { result.expect("driver failure is a known cleanup diagnostic, not process failure"); }
                    drop(application);
                    proxy.release();
                    timeout(Duration::from_secs(3),async { loop {
                        if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1))", &[&vec![original_pid,recovery_pid]]).await.expect("actual cleanup backends ended").get::<_,bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("both original and readback backends ended");
                    let application = Application::start(connections(&server,&name,"clerk"),keys(0),Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("restart with permanent receipt and deleted credentials");
                    let (base,stop,task) = running(application).await;
                    let response = http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id",OPERATION).header("session-id",&session_id).header("cookie",cookie).send().await.expect("complete original credentials after cleanup and restart");
                    assert_eq!(response.status(),401);
                    assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe authentication refusal"),serde_json::json!({"code":"authentication_invalid"}));
                    stop.send(()).expect("stop later cleanup round"); let (application,result)=task.await.expect("later task"); result.expect("later drain"); drop(application);
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.receipts WHERE id=$1 AND operation_id=$2) AND NOT EXISTS(SELECT 1 FROM identity.attempts WHERE id=$1)", &[&receipt_id,&operation]).await.expect("permanent receipt retained across later cleanup").get::<_,bool>(0));
                    observer.terminate().await.expect("observer ended"); admin.terminate().await.expect("owner ended");
                    assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst),1);
                    assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(), "no additional external request through recovery, restart and drain");
                    sms.stop().await; proxy.stop().await;
                }).await.expect("actual retention process budget");
            }
            // The parent owns this 0600 file; fixture tokens never enter captured diagnostics.
            std::fs::write(std::env::var_os("NUCLEUS_TEST_RETENTION_EXPECTATIONS").expect("private fixture path"), serde_json::to_vec(&expected).expect("fixture expectations")).expect("write private expectations");
        });
    }

    #[test]
    fn c1_retention_preserves_completed_deletion_while_stop_waits_for_readback_cleanup() {
        use fixture::database_proxy::{Fault, Proxy};

        if std::env::var_os("NUCLEUS_TEST_C1_RETENTION").is_none() {
            Runtime::new().expect("parent runtime").block_on(async {
                let expected=tempfile::NamedTempFile::new().expect("private fixture expectations");
                let child=tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact","process::tests::c1_retention_preserves_completed_deletion_while_stop_waits_for_readback_cleanup","--nocapture"])
                    .env("NUCLEUS_TEST_C1_RETENTION","1").env("NUCLEUS_TEST_RETENTION_EXPECTATIONS",expected.path())
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true)
                    .spawn().expect("isolated diagnostics process");
                let output=timeout(Duration::from_secs(35),child.wait_with_output()).await.expect("child budget").expect("child output");
                let text=String::from_utf8(output.stderr).expect("JSON diagnostics");
                assert!(output.status.success(),"{text}");
                let expected:serde_json::Value=serde_json::from_slice(&std::fs::read(expected.path()).expect("private fixture readback")).expect("operation and actual code");
                let events:Vec<serde_json::Value>=text.lines().map(|line|serde_json::from_str(line).expect("JSON event")).collect();
                let failures:Vec<_>=events.iter().filter(|event|event["level"]=="ERROR").collect();
                assert_eq!(failures.len(),1,"only the actual cleanup records its final failure: {text}");
                let event=failures[0];
                assert_eq!(event["fields"]["stage"],"retention_completion");
                assert_eq!(event["fields"]["kind"],"unavailable");
                assert!(event["spans"].as_array().expect("original operation span").iter().any(|span|span["operation_id"]==expected["operation"]));
                let error=event["fields"]["error"].as_str().expect("safe original and cleanup reasons");
                for reason in ["RetentionCompleted { cause: RetentionReadback {", "cause: Combined { cause: Query(connection closed)", "commit: Err(CommitTimeout(Elapsed", "termination: Ok(())"] { assert!(error.contains(reason),"missing {reason}: {event}"); }
                assert!(!error.contains("Unknown"));
                for secret in [expected["code"].as_str().expect("actual generated code"),"fictional-retention-token","13800000000","FictionalAccessKey","ACS3-HMAC-SHA256",fixture::PASSWORD,"SELECT ","DELETE ","fictional-sensitive-termination-detail"] {
                    assert!(!events.iter().any(|event|contains_text(event,secret)),"private material in diagnostics");
                }
            });
            return;
        }
        crate::diagnostics::init().expect("actual online diagnostics");
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30),async {
                let name=database(&server).await;
                let supplier=Supplier::responding(serde_json::json!({"success":true,"challenge_ts":chrono::Utc::now(),"hostname":"example.com","action":"send_authentication_code","cdata":OPERATION}).to_string()).await;
                let mut sms=fixture::supplier::Supplier::start().await;
                let mut application=Application::start(connections(&server,&name,"clerk"),keys(0),Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("initial application");
                let service=application.authentication().clone();
                let task=tokio::spawn(async move { service.send(input(OPERATION,"13800000000","fictional-retention-token")).await });
                let request=timeout(Duration::from_secs(5),sms.request()).await.expect("original PNVS request");
                let text=std::str::from_utf8(&request.bytes).expect("fictional request");
                let target=text.lines().next().expect("request line").split(' ').nth(1).expect("request target");
                let url=url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("supplier URL");
                let params=url.query_pairs().find(|(name,_)|name=="TemplateParam").expect("message parameters").1;
                let code=serde_json::from_str::<serde_json::Value>(&params).expect("template parameters")["code"].as_str().expect("actual code").to_owned();
                accept_sms(request);
                let receipt=task.await.expect("original work").expect("sent receipt");
                application.stop().await.expect("original work drained"); drop(application);
                let address=sms.address();
                let authority=sms.authority().to_vec();
                tokio::select! {
                    biased;
                    _=sms.request()=>panic!("cleanup, restart and drain must not call PNVS"),
                    ()=async {
                        let admin=connections(&server,&name,"postgres").recover().await.expect("isolated history owner");
                        admin.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica;
                            UPDATE identity.challenges SET created_at=created_at-interval '25 hours',expires_at=expires_at-interval '25 hours',solved_at=solved_at-interval '25 hours',finished_at=finished_at-interval '25 hours';
                            UPDATE identity.messages SET created_at=created_at-interval '25 hours',expires_at=expires_at-interval '25 hours',finished_at=finished_at-interval '25 hours';
                            UPDATE identity.checks SET expires_at=expires_at-interval '25 hours';
                            UPDATE identity.claims SET created_at=created_at-interval '25 hours'; COMMIT").await.expect("consistent isolated C1 history");
                        let proxy=Proxy::start(&server).await;
                        let application=Application::start(proxy_connections(&name,&proxy),keys(0),Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"),process::Settings::new(Duration::from_millis(30)).expect("short stop"),supplier.client(),message_at(address,&authority)).await.expect("healthy historical startup");
                        let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
                        let row=observer.client().query_one("SELECT challenge.id,challenge.operation_id FROM identity.challenges challenge JOIN identity.messages message ON message.challenge_id=challenge.id WHERE message.id=$1",&[&receipt.id()]).await.expect("original graph from independent SQL");
                        let challenge_id:Uuid=row.get(0);let operation:Uuid=row.get(1);
                        let message_id=receipt.id();
                        std::fs::write(std::env::var_os("NUCLEUS_TEST_RETENTION_EXPECTATIONS").expect("private expectation path"),serde_json::to_vec(&serde_json::json!({"operation":operation,"code":code})).expect("private fixture JSON")).expect("parent-owned 0600 fixture file");
                        admin.client().batch_execute("BEGIN; SELECT pg_advisory_xact_lock(1314210636,1)").await.expect("hold actual retention barrier");
                        let blocker:i32=admin.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("blocker PID").get(0);
                        let (_,stop,task)=running(application).await;
                        let original_pid=timeout(Duration::from_secs(3),async {loop {
                            if let Some(row)=observer.client().query_opt("SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND query='SELECT pg_catalog.pg_advisory_xact_lock_shared(1314210636, 1)' AND $1=ANY(pg_catalog.pg_blocking_pids(pid))",&[&blocker]).await.expect("C1 retention barrier waiter") {break row.get::<_,i32>(0);}
                            tokio::task::yield_now().await;
                        }}).await.expect("C1 reaches barrier after empty R1 scan");
                        proxy.arm(Fault::DelayedCommit);admin.client().batch_execute("COMMIT").await.expect("release original deletion");
                        timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("actual delete COMMIT held");
                        // Independent SHA-256 for the nil C1 operation yields these exact halves.
                        let recovery_pid=timeout(Duration::from_secs(3),async {loop {
                            if let Some(row)=observer.client().query_opt("SELECT pid FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND mode='ExclusiveLock' AND classid=2647903637 AND objid=2112932477 AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND $1=ANY(pg_catalog.pg_blocking_pids(pid))",&[&original_pid]).await.expect("same original operation recovery") {break row.get::<_,i32>(0);}
                            tokio::task::yield_now().await;
                        }}).await.expect("recovery waits for original deletion");
                        let group="SELECT EXISTS(SELECT 1 FROM identity.challenges WHERE id=$1),EXISTS(SELECT 1 FROM identity.messages WHERE id=$2),EXISTS(SELECT 1 FROM identity.checks WHERE id=$2),EXISTS(SELECT 1 FROM identity.claims WHERE id=$2)";
                        let row=observer.client().query_one(group,&[&challenge_id,&message_id]).await.expect("original deletion still uncommitted");assert!((0..4).all(|column|row.get::<_,bool>(column)));
                        proxy.arm(Fault::StalledReadCommit);proxy.release();
                        timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("complete readback COMMIT response held");
                        let row=observer.client().query_one(group,&[&challenge_id,&message_id]).await.expect("independent deletion before stop or next round");assert!((0..4).all(|column|!row.get::<_,bool>(column)));
                        stop.send(()).expect("stop while actual cleanup is owned");
                        let (mut application,result)=timeout(Duration::from_secs(3),task).await.expect("bounded first stop").expect("process task");
                        assert!(matches!(result,Err(process::RunError::Application(process::Error::Timeout{..}))));
                        assert!(instance_held(&server, &name).await);
                        application.settings.stop=Duration::from_secs(15);
                        assert!(timeout(Duration::from_millis(30),application.stop()).await.is_err(),"cancelled stop waiter retains cleanup");
                        application.stop().await.expect("same work drains within its recovery deadline");drop(application);
                        proxy.release();
                        timeout(Duration::from_secs(3),async {loop {
                            if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1))",&[&vec![original_pid,recovery_pid]]).await.expect("actual backend termination").get::<_,bool>(0) {break;}
                            tokio::task::yield_now().await;
                        }}).await.expect("both original and recovery backends ended");
                        let application=Application::start(connections(&server,&name,"clerk"),keys(0),Settings::new(8,domain::identity::message::Limits::default(),session_settings(),domain::registration::Limits::default(),avatar_settings()).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(address,&authority)).await.expect("restart without original C1 facts");
                        let (_,stop,task)=running(application).await;
                        stop.send(()).expect("stop next process round");let (application,result)=task.await.expect("next process task");result.expect("next drain");drop(application);
                        let row=observer.client().query_one(group,&[&challenge_id,&message_id]).await.expect("original IDs remain absent");assert!((0..4).all(|column|!row.get::<_,bool>(column)));
                        assert_eq!(supplier.calls.load(Ordering::SeqCst),1,"no later Siteverify through recovery and drain");
                        observer.terminate().await.expect("observer ended");admin.terminate().await.expect("owner ended");proxy.stop().await;
                    }=>{},
                }
                sms.stop().await;
            }).await.expect("actual cleanup process budget");
        });
    }

    #[test]
    fn failed_clock_connection_releases_startup_resources_before_retry() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let error = Application::start(
                connections(&server, &name, "missing_runtime_identity"),
                keys(0),
                Settings::new(
                    2,
                    domain::identity::message::Limits::default(),
                    session_settings(),
                    domain::registration::Limits::default(),
                    avatar_settings(),
                )
                .expect("settings"),
                process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                supplier.client(),
                message().await,
            )
            .await
            .err()
            .expect("clock connection must succeed before construction");
            assert!(matches!(
                error,
                process::Error::Instance(domain::runtime::Error::Connection(_))
            ));
            let mut application = start(&server, &name, 0, 2, &supplier).await;
            application.stop().await.expect("stop successful retry");
        });
    }

    #[test]
    fn retains_original_acceptance_and_checks_proof_before_input() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let mut service = start(&server, &name, 0, 4, &supplier).await;
            let receipt = service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("first acceptance");
            assert_eq!(receipt.expires_at() - receipt.created_at(), chrono::TimeDelta::seconds(10));
            assert_eq!(Uuid::parse_str(&receipt.id().to_string()).expect("UUID").get_version_num(), 4);
            let retry = Input::new(OPERATION.parse().expect("operation"), "13800000000".parse().expect("mobile"), "fictional-token".parse().expect("token"), "198.51.100.2".parse().expect("new source"));
            assert_eq!(service.authentication().verify(retry).await.expect("original readback"), receipt);
            assert!(matches!(service.authentication().verify(input(OPERATION, "13900000000", "different-token")).await, Err(Error::Proof)));
            assert!(matches!(service.authentication().verify(input(OPERATION, "13900000000", "fictional-token")).await, Err(Error::Mismatch)));
            assert!(matches!(service.authentication().verify(input(OTHER, "13800000000", "fictional-token")).await, Err(Error::Occupied)));
            let read = connections(&server, &name, "clerk").recover().await.expect("inspect");
            let row = read.client().query_one("SELECT count(*) OVER () AS total, purpose, source::text, calls, (mobile).value AS mobile FROM identity.challenges", &[]).await.expect("stored row");
            assert_eq!(row.get::<_, i64>("total"), 1);
            assert_eq!(row.get::<_, String>("purpose"), "authentication");
            assert_eq!(row.get::<_, String>("source"), "192.0.2.1/32");
            assert_eq!(row.get::<_, i16>("calls"), 1);
            assert!(!row.get::<_, Vec<u8>>("mobile").windows(11).any(|bytes| bytes == b"13800000000"));
            read.terminate().await.expect("end inspect");
            service.stop().await.expect("stop");
            assert!(matches!(service.authentication().verify(input(OTHER, "13800000000", "new-token")).await, Err(Error::Closed)));
            drop(service);
            // The clock and original Challenge are unchanged by a service restart.
            let mut service = start(&server, &name, 0, 4, &supplier).await;
            assert_eq!(service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("restart readback"), receipt);
            service.stop().await.expect("stop restart");
        });
    }

    #[test]
    fn concurrent_requests_share_one_fact_and_each_verify_their_input() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let mut service = start(&server, &name, 0, 4, &supplier).await;
            let (first, second) = tokio::join!(
                service
                    .authentication()
                    .verify(input(OPERATION, "13800000000", "fictional-token")),
                service
                    .authentication()
                    .verify(input(OPERATION, "13800000000", "fictional-token"))
            );
            assert_eq!(first.expect("first"), second.expect("second"));
            let (matching, changed) = tokio::join!(
                service
                    .authentication()
                    .verify(input(OPERATION, "13800000000", "fictional-token")),
                service
                    .authentication()
                    .verify(input(OPERATION, "13900000000", "fictional-token"))
            );
            matching.expect("same input");
            assert!(matches!(changed, Err(Error::Mismatch)));
            let (first, second) = tokio::join!(
                service
                    .authentication()
                    .verify(input(OTHER, "13800000000", "another-token")),
                service.authentication().verify(input(
                    "550e8400-e29b-41d4-a716-446655440000",
                    "13800000000",
                    "another-token"
                ))
            );
            assert!(matches!(
                (first, second),
                (Ok(_), Err(Error::Occupied)) | (Err(Error::Occupied), Ok(_))
            ));
            service.stop().await.expect("stop");
        });
    }

    async fn wait_for_lock(client: &tokio_postgres::Client) {
        timeout(Duration::from_secs(5), async {
            loop {
                let waiting: bool = client.query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database()) AND NOT granted)", &[]).await.expect("lock wait").get(0);
                if waiting { break; }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.expect("worker reached database lock");
    }

    #[test]
    fn cancelled_waiter_retains_work_and_stop_preserves_the_instance_until_drop() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let mut service = Application::start(
                connections(&server, &name, "clerk"),
                keys(0),
                Settings::new(
                    1,
                    domain::identity::message::Limits::default(),
                    session_settings(),
                    domain::registration::Limits::default(),
                    avatar_settings(),
                )
                .expect("settings"),
                process::Settings::new(Duration::from_secs(1)).expect("process settings"),
                supplier.client(),
                message().await,
            )
            .await
            .expect("start acceptance");
            let blocker = connections(&server, &name, "postgres")
                .recover()
                .await
                .expect("blocker");
            blocker
                .client()
                .batch_execute("BEGIN; SELECT pg_advisory_xact_lock(1314210636, 1)")
                .await
                .expect("block writes");
            let worker = service.authentication().clone();
            let waiter = tokio::spawn(async move {
                worker
                    .verify(input(OPERATION, "13800000000", "fictional-token"))
                    .await
            });
            wait_for_lock(blocker.client()).await;
            waiter.abort();
            assert!(
                waiter
                    .await
                    .expect_err("waiter cancellation")
                    .is_cancelled()
            );
            assert!(matches!(
                service
                    .authentication()
                    .verify(input(OTHER, "13800000000", "other-token"))
                    .await,
                Err(Error::Busy)
            ));
            assert!(
                timeout(Duration::from_millis(40), service.stop())
                    .await
                    .is_err()
            );
            assert!(instance_held(&server, &name).await);
            assert!(matches!(
                service.stop().await,
                Err(process::Error::Timeout { .. })
            ));
            assert!(instance_held(&server, &name).await);
            blocker
                .client()
                .batch_execute("COMMIT")
                .await
                .expect("release writes");
            service
                .stop()
                .await
                .expect("drain retained handle after cancelled and timed-out stops");
            assert!(instance_released(&server, &name).await);
            let total: i64 = blocker
                .client()
                .query_one("SELECT count(*) FROM identity.challenges", &[])
                .await
                .expect("count")
                .get(0);
            assert_eq!(total, 1);
            blocker.terminate().await.expect("end blocker");
            let authentication = service.authentication().clone();
            drop(service);
            let mut service = start(&server, &name, 0, 1, &supplier).await;
            assert!(matches!(
                authentication
                    .verify(input(OTHER, "13800000000", "other-token"))
                    .await,
                Err(Error::Closed)
            ));
            service
                .authentication()
                .verify(input(OPERATION, "13800000000", "fictional-token"))
                .await
                .expect("lost response readback");
            service.stop().await.expect("stop");
        });
    }

    #[test]
    fn incomplete_stop_reports_once_before_exiting_unsuccessfully() {
        if let Ok(mode) = std::env::var("NUCLEUS_TEST_STOP_DIAGNOSTICS") {
            crate::diagnostics::init().expect("online diagnostics");
            let error = if mode == "retention" {
                let elapsed = Runtime::new().expect("runtime").block_on(async {
                    timeout(Duration::ZERO, std::future::pending::<()>())
                        .await
                        .expect_err("stop deadline")
                });
                process::RunError::Combined {
                    cause: Box::new(process::RunError::Retention(
                        process::retention::Error::Operation(Error::Runtime(
                            domain::runtime::Error::ClockInvalid,
                        )),
                    )),
                    termination: process::Error::Timeout {
                        elapsed,
                        failures: vec![Arc::new(process::Error::Http(
                            process::transport::Error::Transport(
                                std::io::ErrorKind::BrokenPipe.into(),
                            ),
                        ))],
                    },
                }
            } else {
                process::RunError::StopTimeout { cause: None }
            };
            process::require_completion(&Err(error));
            panic!("incomplete shutdown must exit");
        }
        Runtime::new().expect("runtime").block_on(async {
            for mode in ["startup", "retention"] {
                let child = tokio::process::Command::new(
                    std::env::current_exe().expect("test binary"),
                )
                .args([
                    "--exact",
                    "process::tests::incomplete_stop_reports_once_before_exiting_unsuccessfully",
                    "--nocapture",
                ])
                .env("NUCLEUS_TEST_STOP_DIAGNOSTICS", mode)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .expect("isolated exit process");
                let output = timeout(Duration::from_secs(10), child.wait_with_output())
                    .await
                    .expect("exit budget")
                    .expect("child output");
                assert_eq!(output.status.code(), Some(1));
                let text = String::from_utf8(output.stderr).expect("diagnostic text");
                let events: Vec<serde_json::Value> = text
                    .lines()
                    .map(|line| serde_json::from_str(line).expect("JSON event"))
                    .collect();
                assert_eq!(events.len(), 1);
                assert_eq!(events[0]["level"], "ERROR");
                assert_eq!(events[0]["fields"]["stage"], "stop");
                assert_eq!(events[0]["fields"]["kind"], "incomplete");
                assert!(events[0].get("spans").is_none());
                if mode == "retention" {
                    let error = events[0]["fields"]["error"]
                        .as_str()
                        .expect("failure diagnostic");
                    assert!(error.contains("ClockInvalid"));
                    assert!(error.contains("BrokenPipe"));
                    assert!(error.contains("Timeout"));
                }
            }
        });
    }

    #[test]
    fn cancelled_waiter_still_records_the_final_technical_failure() {
        // Python SHA-256 of the two u32-length-prefixed fields:
        // b"challenge/operation", UUID(OPERATION).bytes. Digest prefix: 9dd3cd957df0ca7d.
        const LOCK: i64 = -7_074_084_547_722_163_587;

        async fn pending(client: &tokio_postgres::Client) -> Option<i32> {
            client
                .query_opt(
                    "SELECT waiting.pid FROM pg_catalog.pg_locks waiting
                 JOIN pg_catalog.pg_locks held ON held.locktype = waiting.locktype
                   AND held.database = waiting.database AND held.classid = waiting.classid
                   AND held.objid = waiting.objid AND held.objsubid = waiting.objsubid
                 WHERE waiting.locktype = 'advisory' AND waiting.mode = 'ExclusiveLock'
                   AND NOT waiting.granted AND waiting.classid = $1 AND waiting.objid = $2
                   AND waiting.objsubid = 1 AND waiting.database = (
                       SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())
                   AND held.pid = pg_backend_pid() AND held.mode = 'ExclusiveLock' AND held.granted
                   AND waiting.pid <> held.pid
                   AND held.pid = ANY(pg_catalog.pg_blocking_pids(waiting.pid))",
                    &[&2_647_903_637_u32, &2_112_932_477_u32],
                )
                .await
                .expect("exact original-operation lock")
                .map(|row| row.get("pid"))
        }

        async fn wait_for_original(client: &tokio_postgres::Client) -> i32 {
            timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(pid) = pending(client).await {
                        return pid;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the target worker waits for its original-operation lock")
        }

        async fn deny_insertion(client: &tokio_postgres::Client) {
            // Revoke the actual V2 column grants only after startup verification.
            client.batch_execute(
                "REVOKE INSERT (id, operation_id, purpose, person_id, session_id, transfer_id, hash, key_id, mobile, source, calls, created_at, expires_at) ON identity.challenges FROM clerk; COMMIT",
            ).await.expect("release only the target operation into a write failure");
            let retained: bool = client
                .query_one(
                    "SELECT has_table_privilege('clerk', 'identity.challenges', 'SELECT')
                    AND NOT has_any_column_privilege('clerk', 'identity.challenges', 'INSERT')",
                    &[],
                )
                .await
                .expect("readback remains permitted but insertion is denied")
                .get(0);
            assert!(retained);
        }

        if std::env::var_os("NUCLEUS_TEST_WORKER_DIAGNOSTICS").is_none() {
            Runtime::new().expect("runtime").block_on(async {
                for mode in ["waiter", "http"] {
                    let child = tokio::process::Command::new(
                    std::env::current_exe().expect("test executable"),
                )
                .args([
                    "--exact",
                    "process::tests::cancelled_waiter_still_records_the_final_technical_failure",
                    "--nocapture",
                ])
                .env("NUCLEUS_TEST_WORKER_DIAGNOSTICS", mode)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .expect("isolated diagnostic process");
                    let output = timeout(Duration::from_secs(30), child.wait_with_output())
                        .await
                        .expect("child budget")
                        .expect("child output");
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let diagnostic = String::from_utf8(output.stderr).expect("diagnostic text");
                    let diagnostic: Vec<serde_json::Value> = diagnostic
                        .lines()
                        .map(|line| {
                            serde_json::from_str::<serde_json::Value>(line).expect("JSON event")
                        })
                        .collect();
                    let events: Vec<_> = diagnostic
                        .iter()
                        .filter(|event| event["fields"]["stage"] == "authentication_completion")
                        .collect();
                    assert_eq!(events.len(), 1);
                    assert_eq!(
                        diagnostic
                            .iter()
                            .filter(|event| event["level"] == "ERROR")
                            .count(),
                        1
                    );
                    assert_eq!(events[0]["level"], "ERROR");
                    assert_eq!(events[0]["fields"]["operation_id"], OPERATION);
                    assert_eq!(events[0]["fields"]["kind"], "unavailable");
                    assert_eq!(events[0]["fields"]["outcome"], "not_committed");
                    assert_eq!(
                        events[0]["fields"]["error"],
                        "NotCommitted { cause: ChallengeReadback { cause: Query(SQLSTATE 42501), commit: Ok(()), termination: Ok(()) } }"
                    );
                    let request_id: uuid::Uuid = events[0]["spans"][0]["request_id"]
                        .as_str()
                        .expect("request ID")
                        .parse()
                        .expect("UUID");
                    assert_eq!(request_id.get_version(), Some(uuid::Version::Random));
                    if mode == "http" {
                        assert_eq!(events[0]["spans"][0]["route"], "/api/v1/codes");
                    }
                    for secret in [
                        "13800000000",
                        "fictional-private-token",
                        "fictional-secret",
                        "SELECT",
                        "UPDATE",
                        fixture::PASSWORD,
                    ] {
                        assert!(!diagnostic.iter().any(|event| contains_text(event, secret)));
                    }
                }
            });
            return;
        }
        crate::diagnostics::init().expect("online diagnostics");
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            use tracing::Instrument;

            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let mut application = start(&server, &name, 0, 8, &supplier).await;
            let blocker = connections(&server, &name, "postgres").recover().await.expect("fixture blocker");
            blocker.client().batch_execute("BEGIN").await.expect("original-operation blocker");
            blocker.client().query_one("SELECT pg_catalog.pg_advisory_xact_lock($1)", &[&LOCK]).await.expect("hold exactly this Challenge operation");
            let application = if std::env::var("NUCLEUS_TEST_WORKER_DIAGNOSTICS").expect("child mode") == "http" {
                let (base, stop, task) = running(application).await;
                let waiter = tokio::spawn(http_client().post(format!("{base}/api/v1/codes"))
                    .header("operation-id", OPERATION)
                    .json(&serde_json::json!({"mobile":"13800000000","token":"fictional-private-token"})).send());
                let pid = wait_for_original(blocker.client()).await;
                waiter.abort();
                assert!(waiter.await.expect_err("HTTP caller ended").is_cancelled());
                assert_eq!(pending(blocker.client()).await, Some(pid), "the same worker remains blocked after HTTP cancellation");
                deny_insertion(blocker.client()).await;
                stop.send(()).expect("stop HTTP");
                let (application, result) = timeout(Duration::from_secs(5), task).await.expect("drain budget").expect("supervisor ended");
                result.expect("retained work finishes");
                application
            } else {
                let service = application.authentication().clone();
                let waiter = tokio::spawn(async move {
                    service.verify(input(OPERATION, "13800000000", "fictional-private-token")).await
                }.instrument(tracing::info_span!("request", request_id = %uuid::Uuid::new_v4())));
                let pid = wait_for_original(blocker.client()).await;
                waiter.abort();
                assert!(waiter.await.expect_err("waiter ended").is_cancelled());
                assert_eq!(pending(blocker.client()).await, Some(pid), "the same worker remains blocked after waiter cancellation");
                deny_insertion(blocker.client()).await;
                application.stop().await.expect("wait for the retained worker");
                application
            };
            assert!(matches!(application.phase, process::Phase::Stopped));
            let facts = blocker.client().query_one(
                "SELECT (SELECT count(*) FROM identity.challenges), (SELECT count(*) FROM identity.messages),
                    (SELECT count(*) FROM identity.checks), (SELECT count(*) FROM identity.claims)",
                &[],
            ).await.expect("independent facts before any replay");
            assert_eq!([0, 1, 2, 3].map(|column| facts.get::<_, i64>(column)), [0; 4]);
            assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            blocker.terminate().await.expect("fixture blocker ends");
        });
    }

    #[test]
    fn time_rollback_closes_acceptance_and_survives_failed_business_work() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut service = proxy_service(&supplier, &name, &proxy).await;
            let admin = connections(&server, &name, "steward").recover().await.expect("deployment");
            proxy.arm(fixture::database_proxy::Fault::RewoundCurrent);
            let worker = service.authentication().clone();
            let task = tokio::spawn(async move {
                worker.verify(input(OPERATION, "13800000000", "fictional-token")).await
            });
            timeout(Duration::from_secs(2), proxy.intercepted())
                .await.expect("business transaction observes rewound time");
            proxy.release();
            let error = task.await.expect("worker").expect_err("time rollback closes admission");
            assert!(matches!(error, Error::NotCommitted { cause } if matches!(
                cause.as_ref(),
                Error::ChallengeReadback { cause, commit: Ok(()), termination: Ok(()) }
                    if matches!(cause.as_ref(), Error::Runtime(domain::runtime::Error::ClockInvalid))
            )));
            assert!(matches!(service.authentication().verify(input(OTHER, "13800000000", "other-token")).await, Err(Error::Closed)));
            let total: i64 = admin.client().query_one("SELECT count(*) FROM identity.challenges", &[]).await.expect("count").get(0);
            assert_eq!(total, 0);
            service.stop().await.expect("stop");
            drop(service);
            proxy.stop().await;
            admin.client().batch_execute("UPDATE security.clocks SET observed_at = clock_timestamp() + interval '1 hour'").await.expect("future persisted high-water fixture");
            let error = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(2, domain::identity::message::Limits::default(), session_settings(), domain::registration::Limits::default(), avatar_settings()).expect("settings"), process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                supplier.client(), message().await).await.err().expect("restart detects rollback");
            assert!(matches!(error, process::Error::Services(domain::startup::Error::Runtime(domain::runtime::Error::ClockInvalid))));
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 0);
            let error = admin.client().batch_execute("UPDATE security.clocks SET observed_at = clock_timestamp()").await.expect_err("database rejects time decrease");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            admin.terminate().await.expect("end deployment");
        });
    }

    fn proxy_connections(name: &str, proxy: &fixture::database_proxy::Proxy) -> Connections {
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

    async fn proxy_service(
        supplier: &Supplier,
        name: &str,
        proxy: &fixture::database_proxy::Proxy,
    ) -> Application {
        Application::start(
            proxy_connections(name, proxy),
            keys(0),
            Settings::new(
                4,
                domain::identity::message::Limits::default(),
                session_settings(),
                domain::registration::Limits::default(),
                avatar_settings(),
            )
            .expect("settings"),
            process::Settings::new(Duration::from_secs(30)).expect("process settings"),
            supplier.client(),
            message().await,
        )
        .await
        .expect("start through proxy")
    }

    #[test]
    fn startup_checks_identity_history_and_access_before_accepting_work() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(40), async {
                let supplier = Supplier::new().await;
                for owner in ["identity", "access"] {
                    let name = database(&server).await;
                    let observer = connections(&server, &name, "postgres")
                        .recover().await.expect("isolated fixture observer");
                    if owner == "identity" {
                        observer.client().batch_execute("
                            BEGIN;
                            SET LOCAL session_replication_role = replica;
                            INSERT INTO identity.avatars (id, account_id, digest, size, width, height, released_at)
                            SELECT gen_random_uuid(), gen_random_uuid(), digest, size, width, height, clock_timestamp() - interval '1 hour'
                            FROM identity.avatars CROSS JOIN generate_series(1, 129) WHERE released_at IS NULL;
                            COMMIT;
                            CREATE TEMP TABLE original_avatar AS
                            SELECT id, digest FROM identity.avatars WHERE released_at IS NOT NULL ORDER BY id OFFSET 128 LIMIT 1
                        ").await.expect("retained avatars with absent original accounts");
                        let row = observer.client().query_one("
                            SELECT (SELECT count(*) FROM identity.avatars WHERE id < (SELECT id FROM original_avatar)),
                                (SELECT count(*) FROM identity.avatars avatar WHERE released_at IS NOT NULL
                                    AND NOT EXISTS (SELECT 1 FROM identity.accounts WHERE id = avatar.account_id))
                        ", &[]).await.expect("independent history and page-boundary facts");
                        assert!(row.get::<_, i64>(0) >= 128);
                        assert_eq!(row.get::<_, i64>(1), 129);
                    } else {
                        observer.client().batch_execute("
                            CREATE TEMP TABLE original_permissions AS SELECT id, permissions FROM access.roles WHERE 'institution.roles'=ANY(permissions);
                            CREATE TEMP TABLE original_entries AS SELECT id, permissions FROM access.changes WHERE 'institution.roles'=ANY(permissions)
                        ").await.expect("preserve original complete permission sets");
                    }
                    let mut healthy = start(&server, &name, 0, 2, &supplier).await;
                    healthy.stop().await.expect("valid retained facts start and stop");
                    drop(healthy);
                    observer.client().batch_execute("BEGIN; SET LOCAL session_replication_role = replica")
                        .await.expect("bypass guards only in the isolated corruption fixture");
                    observer.client().batch_execute(if owner == "identity" {
                        "UPDATE identity.avatars SET digest = decode(repeat('00', 32), 'hex') WHERE id = (SELECT id FROM original_avatar)"
                    } else {
                        "UPDATE access.roles SET permissions=array_remove(permissions,'institution.roles') WHERE id IN (SELECT id FROM original_permissions); UPDATE access.changes SET permissions=array_remove(permissions,'institution.roles') WHERE id IN (SELECT id FROM original_entries)"
                    }).await.expect("inject the selected invalid fact");
                    observer.client().batch_execute("COMMIT").await.expect("publish invalid fixture");
                    let error = Application::start(
                        connections(&server, &name, "clerk"), keys(0),
                        Settings::new(2, domain::identity::message::Limits::default(), session_settings(), domain::registration::Limits::default(), avatar_settings()).expect("settings"),
                        process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                        supplier.client(), message().await,
                    ).await.err().expect("invalid retained facts prevent startup");
                    match (owner, error) {
                        ("identity", process::Error::Services(domain::startup::Error::Identity(
                            domain::identity::verification::Error::Facts { rule: "default avatar content" },
                        ))) => {},
                        ("access", process::Error::Services(domain::startup::Error::Role(domain::kernel::Error::Corrupt(fault)))) => {
                            assert_eq!(fault.stage(), domain::kernel::Stage::Command);
                            assert!(matches!(fault.kind(), domain::kernel::FaultKind::History(domain::history::Error::Integrity("role scope, level, system definition and permissions agree"))), "{fault:?}");
                        }
                        (_, error) => panic!("unexpected startup failure: {error:?}"),
                    }
                    assert_startup_released(observer.client()).await;
                    observer.client().batch_execute("BEGIN; SET LOCAL session_replication_role = replica")
                        .await.expect("restore only the isolated fixture");
                    observer.client().batch_execute(if owner == "identity" {
                        "UPDATE identity.avatars avatar SET digest = original.digest FROM original_avatar original WHERE avatar.id = original.id"
                    } else {
                        "UPDATE access.roles role SET permissions=original.permissions FROM original_permissions original WHERE role.id=original.id; UPDATE access.changes change SET permissions=original.permissions FROM original_entries original WHERE change.id=original.id"
                    }).await.expect("restore the original facts");
                    observer.client().batch_execute("COMMIT").await.expect("publish restored fixture");
                    observer.terminate().await.expect("observer ended");
                    let mut restored = start(&server, &name, 0, 2, &supplier).await;
                    restored.stop().await.expect("restored facts start and stop");
                    drop(restored);
                    assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
                }
            }).await.expect("complete startup verification budget");
        });
    }

    async fn assert_startup_released(client: &tokio_postgres::Client) {
        timeout(Duration::from_secs(5), async {
            loop {
                client.simple_query("SELECT pg_stat_clear_snapshot()").await.expect("refresh backend observation");
                let ended: bool = client.query_one("SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname = current_database() AND usename = 'clerk')", &[])
                    .await.expect("independent startup backend observation").get(0);
                if ended {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }).await.expect("all startup backends ended");
        assert!(
            client
                .query_one("SELECT pg_try_advisory_xact_lock(1314210636, 1)", &[])
                .await
                .expect("independent startup barrier observation")
                .get::<_, bool>(0)
        );
        assert!(
            client
                .query_one("SELECT pg_try_advisory_lock(1314210636, 3)", &[])
                .await
                .expect("independent instance lock observation")
                .get::<_, bool>(0),
            "instance lock released"
        );
        client
            .query_one("SELECT pg_advisory_unlock(1314210636, 3)", &[])
            .await
            .expect("release the observation lock");
    }

    #[test]
    fn startup_timeout_reports_the_step_once_without_private_values() {
        use domain::database::verification::{Stage, Waiting};
        use fixture::database_proxy::{Fault, Proxy};
        use serde_json::Value;
        use tokio::time::Instant;

        if std::env::var_os("NUCLEUS_TEST_STARTUP_TIMEOUT_DIAGNOSTICS").is_none() {
            Runtime::new().expect("runtime").block_on(async {
                for (scenario, stage, waiting, connection) in [
                    ("clock_connection", "clock_connection", "connection", "acquiring"),
                    ("clock_connect_budget", "clock_connection", "connection", "acquiring"),
                    ("barrier_connection", "barrier_connection", "connection", "acquiring"),
                    ("barrier_deadline", "barrier_connection", "connection", "acquiring"),
                    ("database_prerequisites", "database_prerequisites", "query_and_validation", "held"),
                    ("trusted_time", "trusted_time", "query", "held"),
                    ("revisions", "revisions", "query", "held"),
                    ("number_states", "history_states", "query_and_validation", "held"),
                ] {
                    let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                        .args(["--exact", "process::tests::startup_timeout_reports_the_step_once_without_private_values", "--nocapture"])
                        .env("NUCLEUS_TEST_STARTUP_TIMEOUT_DIAGNOSTICS", scenario)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::piped())
                        .kill_on_drop(true)
                        .spawn().expect("isolated diagnostics child");
                    let output = timeout(Duration::from_secs(45), child.wait_with_output())
                        .await.expect("diagnostics child deadline").expect("child output");
                    let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
                    assert!(output.status.success(), "scenario {scenario}: {text}");
                    let events: Vec<Value> = text.lines()
                        .map(|line| serde_json::from_str(line).expect("JSON event")).collect();
                    let timeouts: Vec<_> = events.iter()
                        .filter(|event| event["fields"]["operation"] == "startup_verification").collect();
                    assert_eq!(timeouts.len(), 1, "one observation for the timed out step: {text}");
                    let event = timeouts[0];
                    assert_eq!(event["target"], "domain::startup");
                    assert_eq!(event["fields"]["stage"], stage);
                    assert_eq!(event["fields"]["kind"], "timeout");
                    assert_eq!(event["fields"]["connection"], connection);
                    assert_eq!(event["fields"]["waiting"], waiting);
                    for secret in ["SELECT", "clock_timestamp", "security.clocks", "pg_advisory_xact_lock", "identity.revisions", "workforce.allocations", "13800000000", "fictional-token", fixture::PASSWORD] {
                        assert!(!events.iter().any(|event| contains_text(event, secret)), "unsafe diagnostic field");
                    }
                }
            });
            return;
        }
        let scenario = std::env::var("NUCLEUS_TEST_STARTUP_TIMEOUT_DIAGNOSTICS")
            .expect("diagnostics scenario");
        let stage = match scenario.as_str() {
            "clock_connection" | "clock_connect_budget" => Stage::ClockConnection,
            "barrier_connection" | "barrier_deadline" => Stage::BarrierConnection,
            "database_prerequisites" => Stage::DatabasePrerequisites,
            "trusted_time" => Stage::TrustedTime,
            "revisions" => Stage::Revisions,
            "number_states" => Stage::HistoryStates,
            _ => panic!("unknown diagnostics scenario"),
        };
        crate::diagnostics::init().expect("production log filter");
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let proxy = Proxy::start(&server).await;
                match stage {
                    Stage::ClockConnection | Stage::BarrierConnection => {},
                    Stage::DatabasePrerequisites => proxy.arm(Fault::StalledDatabasePrerequisites),
                    Stage::TrustedTime => proxy.arm(Fault::StalledTime),
                    Stage::Revisions => proxy.arm(Fault::StalledIdentityRevisions),
                    Stage::HistoryStates => proxy.arm(Fault::StalledNumberStates),
                    _ => unreachable!("selected diagnostics scenario"),
                }
                // Relay normal connections, but hold the selected SSL negotiation before TLS.
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("connection gate");
                let port = listener.local_addr().expect("gate address").port();
                let upstream = proxy.port();
                let (entered, intercepted) = tokio::sync::oneshot::channel();
                let gate = tokio::spawn(async move {
                    use tokio::io::AsyncReadExt;

                    let mut relays = tokio::task::JoinSet::new();
                    for index in 0.. {
                        let (mut socket, _) = listener.accept().await.expect("startup connection");
                        if (stage == Stage::ClockConnection && index == 0)
                            || (stage == Stage::BarrierConnection && index == 1)
                        {
                            socket.read_exact(&mut [0; 8]).await.expect("SSL negotiation reached");
                            entered.send(()).expect("connection interception observed");
                            std::future::pending::<()>().await;
                            return;
                        }
                        relays.spawn(async move {
                            let mut backend = tokio::net::TcpStream::connect(("127.0.0.1", upstream)).await.expect("fixture proxy");
                            let _ = tokio::io::copy_bidirectional(&mut socket, &mut backend).await;
                        });
                    }
                });
                let connect_budget = match scenario.as_str() {
                    "clock_connect_budget" => Duration::from_millis(200),
                    "barrier_deadline" => Duration::from_secs(20),
                    _ => Duration::from_secs(5),
                };
                let mut roots = RootCertStore::empty();
                roots.add(proxy.authority()).expect("proxy trust anchor");
                let startup_connections = Connections::new(
                    database::Settings::new("127.0.0.1".into(), port, name.clone(), "clerk".into(), 2,
                        database::settings::Budgets::new(Duration::from_secs(3), connect_budget, Duration::from_secs(3)).expect("connection budgets"))
                        .expect("proxy settings"),
                    Password::new(fixture::PASSWORD.into()).expect("password"), roots,
                ).expect("proxy connections");
                let startup = Box::pin(Application::start(
                    startup_connections,
                    keys(0),
                    Settings::new(
                        2,
                        domain::identity::message::Limits::default(),
                        session_settings(),
                        domain::registration::Limits::default(),
                        avatar_settings(),
                    )
                    .expect("settings"),
                    process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                    supplier.client(),
                    message().await,
                ));
                // The prerequisite deadline includes context construction and recovery.
                // Allow one second for scheduler latency and connection termination.
                let bound = match stage {
                    Stage::ClockConnection => 4,
                    Stage::BarrierConnection | Stage::DatabasePrerequisites => 11,
                    _ => 15,
                };
                let ((result, finished_at), intercepted_at) = timeout(Duration::from_secs(bound), async {
                    tokio::join!(async {
                        let result = startup.await;
                        (result, Instant::now())
                    }, async {
                        timeout(Duration::from_secs(5), async {
                            if matches!(stage, Stage::ClockConnection | Stage::BarrierConnection) {
                                intercepted.await.expect("SSL negotiation intercepted");
                            } else {
                                proxy.intercepted().await;
                            }
                        })
                            .await
                            .expect("the selected startup wait actually reached the proxy");
                        Instant::now()
                    })
                }).await.expect("startup fails within its existing bound");
                if stage == Stage::TrustedTime {
                    // Exclude preceding startup work; allow one second for cleanup and scheduling.
                    let elapsed = finished_at.duration_since(intercepted_at);
                    assert!(elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(4), "{elapsed:?}");
                }
                let error = result.err().expect("stalled query fails startup");
                match stage {
                    Stage::ClockConnection => assert!(matches!(
                        &error,
                        process::Error::Instance(domain::runtime::Error::Timeout(_)
                            | domain::runtime::Error::Connection(platform::database::Error::Connection(platform::database::ConnectionError::Timeout(_))))
                    ), "{error:?}"),
                    Stage::BarrierConnection => assert!(matches!(
                        &error,
                        process::Error::Database(domain::database::Error::Timeout(cause)) if cause.stage() == stage
                    ) || matches!(&error, process::Error::Database(domain::database::Error::Connection(
                        platform::database::Error::Connection(platform::database::ConnectionError::Timeout(_))
                    ))), "{error:?}"),
                    Stage::TrustedTime => assert!(matches!(
                        &error,
                        process::Error::Services(domain::startup::Error::Runtime(domain::runtime::Error::Timeout(_)))
                    ), "{error:?}"),
                    Stage::HistoryStates => assert!(matches!(
                        &error,
                        process::Error::Services(domain::startup::Error::Number(
                            domain::workforce::number::Error::Unavailable(fault)
                        )) if matches!(fault.kind(), domain::kernel::FaultKind::History(domain::history::Error::Timeout(cause)) if cause.stage() == stage && cause.waiting() == Waiting::QueryAndValidation)
                    ), "{error:?}"),
                    // The enclosing deadline can also expire during connection termination,
                    // after the stage has already emitted its diagnostic.
                    Stage::DatabasePrerequisites => assert!(matches!(
                        &error,
                        process::Error::Database(domain::database::Error::Timeout(cause))
                            if cause.stage() == stage && cause.waiting() == Waiting::QueryAndValidation
                    ) || matches!(&error, process::Error::Timeout { .. }), "{error:?}"),
                    Stage::Revisions => {
                        assert!(matches!(&error, process::Error::Services(domain::startup::Error::Identity(
                            domain::identity::verification::Error::Revision(domain::identity::person::revision::Error::History(
                                domain::history::Error::Timeout(cause)
                            ))
                        )) if cause.stage() == stage && cause.waiting() == Waiting::Query));
                        assert!(format!("{error:?}").contains("Revisions"));
                        assert_eq!(error.to_string(), "history startup verification exceeded its batch budget");
                    }
                    _ => unreachable!("selected diagnostics scenario"),
                }
                proxy.release();
                let observer = connections(&server, &name, "postgres")
                    .recover()
                    .await
                    .expect("observer");
                assert_startup_released(observer.client()).await;
                observer.terminate().await.expect("observer ended");
                gate.abort();
                assert!(gate.await.expect_err("connection gate stopped").is_cancelled());
                proxy.stop().await;
                let mut application = start(&server, &name, 0, 2, &supplier).await;
                application
                    .stop()
                    .await
                    .expect("restart after original resources ended");
                assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            })
            .await
            .expect("bounded startup timeout and recovery");
        });
    }

    #[test]
    fn a_blocked_identity_batch_fails_startup_and_releases_its_resources() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let blocker = connections(&server, &name, "postgres")
                    .recover()
                    .await
                    .expect("isolated lock owner and observer");
                blocker
                    .client()
                    .batch_execute("BEGIN; LOCK TABLE identity.revisions IN ACCESS EXCLUSIVE MODE")
                    .await
                    .expect("block identity reconstruction");
                let startup = Application::start(
                    connections(&server, &name, "clerk"),
                    keys(0),
                    Settings::new(
                        2,
                        domain::identity::message::Limits::default(),
                        session_settings(),
                        domain::registration::Limits::default(),
                        avatar_settings(),
                    )
                    .expect("settings"),
                    process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                    supplier.client(),
                    message().await,
                );
                let (result, ()) = tokio::join!(startup, async {
                    timeout(Duration::from_secs(5), async {
                        loop {
                            blocker
                                .client()
                                .simple_query("SELECT pg_stat_clear_snapshot()")
                                .await
                                .expect("refresh blocked query observation");
                            let blocked: bool = blocker
                                .client()
                                .query_one(
                                    "
                                SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity
                                    WHERE datname = current_database() AND usename = 'clerk'
                                        AND pg_backend_pid() = ANY(pg_blocking_pids(pid))
                                        AND query LIKE 'SELECT record.person_id, record.sequence,%FROM identity.changes record WHERE%')
                            ",
                                    &[],
                                )
                                .await
                                .expect("observe actual identity query waiting on the table lock")
                                .get(0);
                            if blocked {
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("startup reached the Person record batch that reads revision sources");
                });
                let error = result
                    .err()
                    .expect("blocked identity batch rejects startup");
                // PostgreSQL and the client each enforce ten seconds; either may expire first.
                match error {
                    process::Error::Services(domain::startup::Error::Identity(
                        domain::identity::verification::Error::History(
                            domain::history::Error::Timeout(ref error),
                        ),
                    )) if error.stage() == domain::database::verification::Stage::HistoryRecords => {}
                    process::Error::Services(domain::startup::Error::Identity(
                        domain::identity::verification::Error::History(
                            domain::history::Error::Query(ref error),
                        ),
                    )) if error.code()
                        == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED) => {}
                    error => panic!("unexpected startup failure: {error:?}"),
                }
                // The table remains locked while the original startup backend is observed ending.
                assert_startup_released(blocker.client()).await;
                blocker
                    .client()
                    .batch_execute("ROLLBACK")
                    .await
                    .expect("release fixture lock");
                blocker.terminate().await.expect("blocker ended");
                let mut application = start(&server, &name, 0, 2, &supplier).await;
                application
                    .stop()
                    .await
                    .expect("startup can retry after original resources end");
                assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            })
            .await
            .expect("bounded identity startup and cleanup");
        });
    }

    #[test]
    fn retained_history_can_take_longer_than_one_batch_budget_to_verify() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let mut application = start(&server, &name, 0, 2, &supplier).await;
            let original = application
                .authentication()
                .verify(input(OPERATION, "13800000000", "fictional-token"))
                .await
                .expect("original acceptance");
            for number in 0..128 {
                application
                    .authentication()
                    .verify(input(
                        &Uuid::new_v4().to_string(),
                        "13800000000",
                        &format!("fictional-token-{number}"),
                    ))
                    .await
                    .expect("retained acceptance");
            }
            application.stop().await.expect("stop original application");
            drop(application);
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            proxy.arm(fixture::database_proxy::Fault::StalledHistory);
            let delays = async {
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("first history batch");
                tokio::time::sleep(Duration::from_secs(6)).await;
                // Arm the next fetch before the current fetch is released.
                proxy.arm(fixture::database_proxy::Fault::StalledHistory);
                proxy.release();
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("second history batch");
                tokio::time::sleep(Duration::from_secs(6)).await;
                proxy.release();
            };
            let (mut application, ()) = timeout(Duration::from_secs(20), async {
                tokio::join!(proxy_service(&supplier, &name, &proxy), delays)
            })
            .await
            .expect("all batches finish within their individual budgets");
            assert_eq!(
                application
                    .authentication()
                    .verify(input(OPERATION, "13800000000", "fictional-token"))
                    .await
                    .expect("original readback after complete scan"),
                original
            );
            application.stop().await.expect("stop verified application");
            proxy.stop().await;
        });
    }

    #[test]
    fn a_stalled_history_batch_fails_startup_and_releases_its_resources() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            proxy.arm(fixture::database_proxy::Fault::StalledHistory);
            let startup = Application::start(
                proxy_connections(&name, &proxy),
                keys(0),
                Settings::new(
                    2,
                    domain::identity::message::Limits::default(),
                    session_settings(),
                    domain::registration::Limits::default(),
                    avatar_settings(),
                )
                .expect("settings"),
                process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                supplier.client(),
                message().await,
            );
            let (result, ()) = timeout(Duration::from_secs(15), async {
                tokio::join!(startup, async {
                    timeout(Duration::from_secs(5), proxy.intercepted())
                        .await
                        .expect("history fetch intercepted");
                })
            })
            .await
            .expect("stalled batch is bounded");
            assert!(matches!(
                result,
                Err(process::Error::Services(
                    domain::startup::Error::Authentication(Error::VerificationTimeout(ref error))
                ))
                if error.stage() == domain::database::verification::Stage::ChallengesBatch
                    && error.waiting() == domain::database::verification::Waiting::QueryAndValidation
            ));
            proxy.stop().await;
            let mut application = start(&server, &name, 0, 2, &supplier).await;
            application
                .stop()
                .await
                .expect("retry after original resources end");
        });
    }

    #[test]
    fn resolves_lost_commit_replies_without_repeating_the_insert() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut service = proxy_service(&supplier, &name, &proxy).await;
            proxy.arm(fixture::database_proxy::Fault::LostCommit);
            let worker = service.authentication().clone();
            let task = tokio::spawn(async move {
                worker
                    .verify(input(OPERATION, "13800000000", "fictional-token"))
                    .await
            });
            timeout(Duration::from_secs(5), proxy.intercepted())
                .await
                .expect("COMMIT intercepted");
            let receipt = timeout(Duration::from_secs(5), task)
                .await
                .expect("recovery completes")
                .expect("worker")
                .expect("confirmed original commit");
            proxy.release();
            assert_eq!(
                service
                    .authentication()
                    .verify(input(OPERATION, "13800000000", "fictional-token"))
                    .await
                    .expect("retry"),
                receipt
            );
            let read = connections(&server, &name, "clerk")
                .recover()
                .await
                .expect("read");
            assert_eq!(
                read.client()
                    .query_one("SELECT count(*) FROM identity.challenges", &[])
                    .await
                    .expect("count")
                    .get::<_, i64>(0),
                1
            );
            read.terminate().await.expect("end read");
            service.stop().await.expect("stop");
            proxy.stop().await;
        });
    }

    #[test]
    fn recovery_waits_for_late_backend_commit_without_stopping_unrelated_operations() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut service = proxy_service(&supplier, &name, &proxy).await;
            proxy.arm(fixture::database_proxy::Fault::DelayedCommit);
            let worker = service.authentication().clone();
            let mut task = tokio::spawn(async move {
                worker
                    .verify(input(OPERATION, "13800000000", "fictional-token"))
                    .await
            });
            timeout(Duration::from_secs(5), proxy.intercepted())
                .await
                .expect("late COMMIT held");
            let admin = connections(&server, &name, "postgres")
                .recover()
                .await
                .expect("observer");
            wait_for_lock(admin.client()).await;
            assert!(timeout(Duration::from_millis(30), &mut task).await.is_err());
            timeout(
                Duration::from_secs(3),
                service
                    .authentication()
                    .verify(input(OTHER, "13900000000", "other-token")),
            )
            .await
            .expect("unrelated operation remains available")
            .expect("independent acceptance");
            proxy.release();
            let receipt = timeout(Duration::from_secs(5), task)
                .await
                .expect("barrier released")
                .expect("worker")
                .expect("late original commit");
            assert_eq!(
                service
                    .authentication()
                    .verify(input(OPERATION, "13800000000", "fictional-token"))
                    .await
                    .expect("original fact"),
                receipt
            );
            admin.terminate().await.expect("end observer");
            service.stop().await.expect("stop");
            proxy.stop().await;
        });
    }

    #[test]
    fn confirmed_rollback_allows_a_new_attempt_and_shutdown_persists_time() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut service = proxy_service(&supplier, &name, &proxy).await;
            let read = connections(&server, &name, "clerk")
                .recover()
                .await
                .expect("read");
            let before: chrono::DateTime<chrono::Utc> = read
                .client()
                .query_one("SELECT observed_at FROM security.clocks", &[])
                .await
                .expect("clock")
                .get(0);
            proxy.arm(fixture::database_proxy::Fault::RolledBack);
            let result = timeout(
                Duration::from_secs(5),
                service
                    .authentication()
                    .verify(input(OPERATION, "13800000000", "fictional-token")),
            )
            .await
            .expect("recovery finished");
            assert!(matches!(result, Err(Error::NotCommitted { .. })));
            assert_eq!(
                read.client()
                    .query_one("SELECT count(*) FROM identity.challenges", &[])
                    .await
                    .expect("count")
                    .get::<_, i64>(0),
                0
            );
            service
                .authentication()
                .verify(input(OPERATION, "13800000000", "fictional-token"))
                .await
                .expect("explicit retry after known rollback");
            service.stop().await.expect("stop");
            let after: chrono::DateTime<chrono::Utc> = read
                .client()
                .query_one("SELECT observed_at FROM security.clocks", &[])
                .await
                .expect("persisted high water after shutdown")
                .get(0);
            assert!(after > before);
            read.terminate().await.expect("end read");
            proxy.stop().await;
        });
    }

    #[test]
    fn stalled_transaction_time_has_one_budget_and_preserves_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut service = proxy_service(&supplier, &name, &proxy).await;
            let observer = connections(&server, &name, "postgres")
                .recover().await.expect("original backend observer");
            proxy.arm(fixture::database_proxy::Fault::StalledTime);
            let worker = service.authentication().clone();
            let task = tokio::spawn(async move {
                worker
                    .verify(input(OPERATION, "13800000000", "fictional-token"))
                    .await
            });
            let pid = timeout(Duration::from_secs(5), proxy.intercepted())
                .await
                .expect("time sample held");
            timeout(Duration::from_secs(5), async {
                while !observer.client().query_one(
                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_catalog.pg_blocking_pids(pid)))",
                    &[&pid],
                ).await.expect("readback waits for the original transaction").get::<_, bool>(0) {
                    tokio::task::yield_now().await;
                }
            }).await.expect("the time budget expires before the ten-second transaction budget");
            assert!(!task.is_finished(), "local timeout does not prove the original backend ended");
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 0);
            proxy.release();
            assert!(matches!(
                timeout(Duration::from_secs(5), task)
                    .await
                    .expect("sample timeout")
                    .expect("worker"),
                Err(Error::NotCommitted { .. })
            ));
            service
                .services
                .ready()
                .await
                .expect("ordinary query timeout preserves admission");
            service
                .authentication()
                .verify(input(OTHER, "13800000000", "other-token"))
                .await
                .expect("a new explicit request can proceed");
            service.stop().await.expect("stop");
            observer.terminate().await.expect("observer ended");
            proxy.stop().await;
        });
    }

    #[test]
    fn retained_token_versions_and_original_deadlines_survive_key_selection_changes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let mut service = start(&server, &name, 0, 2, &supplier).await;
            let original = service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("original");
            service.stop().await.expect("stop");
            drop(service);
            let owner = connections(&server, &name, "steward").recover().await.expect("owner");
            // Rotation switches every use with the root, so the offline fixture also recomputes the
            // active-only lookups the way key maintenance must; token proofs keep root 0.
            let rotated = keys(1);
            let challenge = rotated.identity.mobile.hash(&"13800000000".parse().expect("mobile")).expect("new-root lookup");
            let number = "000000200002290010".parse().expect("identity number");
            let admin = connections(&server, &name, "postgres").recover().await.expect("offline maintenance fixture");
            let account = admin.client().query_one("SELECT id,jsonb_build_array(person_id,avatar,wechat,created_at,changed_at,suspension_id,cancellation_id,sequence,version)::text AS business FROM identity.accounts",&[]).await.expect("one bootstrap account");
            let account_id:domain::identity::account::Id=account.get("id");
            let original_business:String=account.get("business");
            admin.client().batch_execute("BEGIN; SET LOCAL session_replication_role = replica; UPDATE security.keys SET active = false WHERE active; UPDATE security.keys SET active = true WHERE id = 1").await.expect("offline key fixture");
            admin.client().execute("UPDATE identity.challenges SET mobile.hash = $1", &[&challenge]).await.expect("recomputed challenge lookup");
            domain::identity::account::rekey_for_test(admin.client(),&keys(0),&rotated,account_id,&number).await.expect("authenticated account lookups and state seal");
            admin.client().batch_execute("COMMIT").await.expect("rotated root");
            let unchanged=admin.client().query_one("SELECT jsonb_build_array(person_id,avatar,wechat,created_at,changed_at,suspension_id,cancellation_id,sequence,version)::text AS business,(mobile).hash=$2 AND id_number=$3 AND seal_key_id=1 AS rekeyed FROM identity.accounts WHERE id=$1",&[&account_id,&rotated.identity.mobile.hash(&"13900000000".parse().expect("mobile")).expect("active mobile lookup"),&rotated.identity.number.hash(&number).expect("active number lookup")]).await.expect("technical conversion preserves the original business facts");
            assert_eq!(unchanged.get::<_,String>("business"),original_business);
            assert!(unchanged.get::<_,bool>("rekeyed"));
            admin.terminate().await.expect("end fixture admin");
            let mut service = start(&server, &name, 1, 2, &supplier).await;
            assert_eq!(service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("original key"), original);
            assert!(matches!(service.authentication().verify(input(OTHER, "13800000000", "fictional-token")).await, Err(Error::Occupied)));
            service.stop().await.expect("stop");
            drop(service);
            // Expired stored facts remain valid to decode and never acquire a fresh deadline.
            owner.client().batch_execute("ALTER TABLE identity.challenges DISABLE TRIGGER challenges_transition_check; UPDATE identity.challenges SET created_at = created_at - interval '1 hour', expires_at = expires_at - interval '1 hour'; ALTER TABLE identity.challenges ENABLE TRIGGER challenges_transition_check").await.expect("expired history fixture");
            let mut service = start(&server, &name, 1, 2, &supplier).await;
            let restored = service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("expired original receipt");
            assert_eq!(restored.id(), original.id());
            assert_eq!(restored.created_at(), original.created_at() - chrono::TimeDelta::hours(1));
            assert_eq!(restored.expires_at(), original.expires_at() - chrono::TimeDelta::hours(1));
            service.stop().await.expect("stop");
            owner.terminate().await.expect("end owner");
        });
    }

    #[test]
    fn startup_rejects_mismatched_root_metadata_without_repairing_it() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let mut application = start(&server, &name, 0, 2, &supplier).await;
            application
                .stop()
                .await
                .expect("root metadata starts and stops");
            drop(application);
            let owner = connections(&server, &name, "steward")
                .recover()
                .await
                .expect("owner");
            let registered = || async {
                let row = owner
                    .client()
                    .query_one(
                        "SELECT jsonb_agg(jsonb_build_array(id, encode(fingerprint, 'hex'), active) ORDER BY id)::text FROM security.keys",
                        &[],
                    )
                    .await
                    .expect("registered root versions");
                row.get::<_, String>(0)
            };
            let original = registered().await;
            // Python hashlib over LP32("nucleus/identity/key/v1", i64be(id), "root", material)
            // for the fixture roots of 32 bytes of 1 and of 2.
            assert_eq!(
                original,
                r#"[[0, "bc97ee04387d5de8793277ebfed69a12d8d6ab60cefdfe437337f6e255aeae86", true], [1, "4c5e5e4ce7b974404ff596ae15187159e0f59a9bfb2cf99644203ffe662094ba", false]]"#
            );
            // Former per-purpose fingerprints of the same material, a missing and an extra
            // version, and another active version all block startup unchanged.
            for (damage, repair) in [
                (
                    "UPDATE security.keys SET fingerprint = decode('f5057f811691ecf5d0cb0d9ad020a66948b3755332a682f9b394511eaae03bff', 'hex') WHERE id = 0",
                    "UPDATE security.keys SET fingerprint = decode('bc97ee04387d5de8793277ebfed69a12d8d6ab60cefdfe437337f6e255aeae86', 'hex') WHERE id = 0",
                ),
                (
                    "UPDATE security.keys SET fingerprint = decode('73b88c7000fa7402b225a894905cc62a61c00d2fb1f310a83a0542930a954b9e', 'hex') WHERE id = 0",
                    "UPDATE security.keys SET fingerprint = decode('bc97ee04387d5de8793277ebfed69a12d8d6ab60cefdfe437337f6e255aeae86', 'hex') WHERE id = 0",
                ),
                (
                    "DELETE FROM security.keys WHERE id = 1",
                    "INSERT INTO security.keys VALUES (1, decode('4c5e5e4ce7b974404ff596ae15187159e0f59a9bfb2cf99644203ffe662094ba', 'hex'), false)",
                ),
                (
                    "INSERT INTO security.keys VALUES (9, decode(repeat('00', 32), 'hex'), false)",
                    "DELETE FROM security.keys WHERE id = 9",
                ),
                (
                    "BEGIN; UPDATE security.keys SET active = false WHERE id = 0; UPDATE security.keys SET active = true WHERE id = 1; COMMIT",
                    "BEGIN; UPDATE security.keys SET active = false WHERE id = 1; UPDATE security.keys SET active = true WHERE id = 0; COMMIT",
                ),
            ] {
                owner.client().batch_execute(damage).await.expect("mismatched metadata fixture");
                let damaged = registered().await;
                let error = Application::start(
                    connections(&server, &name, "clerk"),
                    keys(0),
                    Settings::new(
                        2,
                        domain::identity::message::Limits::default(),
                        session_settings(),
                        domain::registration::Limits::default(),
                        avatar_settings(),
                    )
                    .expect("settings"),
                    process::Settings::new(Duration::from_secs(30)).expect("stop settings"),
                    supplier.client(),
                    message().await,
                )
                .await
                .err()
                .expect("mismatched metadata blocks startup");
                assert!(matches!(
                    error,
                    process::Error::Services(domain::startup::Error::Metadata(
                        domain::keys::database::Error::Mismatch
                    ))
                ), "{damage}");
                assert_eq!(registered().await, damaged, "startup never repairs metadata");
                owner.client().batch_execute(repair).await.expect("restore root metadata");
                assert_eq!(registered().await, original);
            }
            let error = owner
                .client()
                .batch_execute("UPDATE security.keys SET active = true")
                .await
                .expect_err("a second active version is structurally rejected");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION));
            owner.terminate().await.expect("end owner");
            let mut application = start(&server, &name, 0, 2, &supplier).await;
            application
                .stop()
                .await
                .expect("restored metadata starts and stops");
            assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn startup_rejects_future_events_missing_clock_and_unauthenticated_history() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            for (index, corruption) in [
                "UPDATE identity.challenges SET created_at = created_at + interval '1 hour', expires_at = expires_at + interval '1 hour', finished_at = finished_at + interval '1 hour'",
                "DELETE FROM security.clocks",
                "UPDATE identity.challenges SET mobile.value = set_byte((mobile).value, octet_length((mobile).value) - 1, get_byte((mobile).value, octet_length((mobile).value) - 1) # 1)",
                "UPDATE identity.challenges SET mobile.hash = decode(repeat('00', 32), 'hex')",
            ].into_iter().enumerate() {
                let name = database(&server).await;
                let mut service = start(&server, &name, 0, 2, &supplier).await;
                service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("original");
                service.stop().await.expect("stop");
                drop(service);
                let owner = connections(&server, &name, "steward").recover().await.expect("owner");
                owner.client().batch_execute("ALTER TABLE identity.challenges DISABLE TRIGGER challenges_transition_check").await.expect("owner creates corrupt fixture");
                owner.client().batch_execute(corruption).await.expect("invalid retained-history fixture");
                owner.client().batch_execute("ALTER TABLE identity.challenges ENABLE TRIGGER challenges_transition_check").await.expect("restore guard");
                owner.terminate().await.expect("end owner");
                let error = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(2, domain::identity::message::Limits::default(), session_settings(), domain::registration::Limits::default(), avatar_settings()).expect("settings"), process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                    supplier.client(), message().await).await.err().expect("startup fails closed");
                assert!(matches!(error, process::Error::Services(domain::startup::Error::Runtime(domain::runtime::Error::ClockInvalid) | domain::startup::Error::Authentication(Error::Mobile(_) | Error::Facts { .. } | Error::Time { .. }))), "{error:?}");
                if index == 0 {
                    assert!(matches!(&error, process::Error::Services(domain::startup::Error::Authentication(Error::Time {
                        stage: domain::identity::authentication::Stage::ChallengeHistory, rule,
                    })) if rule.contains("created_at")));
                }
                if index == 1 {
                    assert!(matches!(error, process::Error::Services(domain::startup::Error::Runtime(domain::runtime::Error::ClockInvalid))));
                }
                assert!(!format!("{error:?} {error}").contains("13800000000"));
                assert!(instance_released(&server, &name).await, "failed startup finished cleanup");
            }
        });
    }

    #[test]
    fn runtime_permissions_are_minimal_and_startup_rejects_direct_and_inherited_excess() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let mut service = start(&server, &name, 0, 2, &supplier).await;
            service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("valid runtime insert");
            service.stop().await.expect("stop");
            drop(service);
            let runtime = connections(&server, &name, "clerk").recover().await.expect("runtime");
            for sql in [
                "TRUNCATE identity.challenges", "UPDATE identity.challenges SET source = inet '192.0.2.2'",
                "DELETE FROM security.clocks", "INSERT INTO security.clocks VALUES (true, clock_timestamp())",
                "UPDATE security.clocks SET id = true", "CREATE TABLE identity.invalid (id integer)",
                "UPDATE security.keys SET fingerprint = decode(repeat('00', 32), 'hex')",
                "UPDATE security.keys SET active = false",
                "DELETE FROM security.keys",
                "INSERT INTO security.keys VALUES (9, decode(repeat('00', 32), 'hex'), false)",
                "INSERT INTO identity.authentications (transaction_id) VALUES (pg_current_xact_id())",
                "INSERT INTO identity.attempts (transaction_id) VALUES (pg_current_xact_id())",
                "INSERT INTO identity.receipts (transaction_id) VALUES (pg_current_xact_id())",
                "DELETE FROM identity.persons",
                "DELETE FROM identity.origins",
                "UPDATE identity.origins SET created_at=created_at",
                "DELETE FROM identity.corrections",
                "UPDATE identity.corrections SET corrected_at=corrected_at",
                "INSERT INTO identity.revisions (transaction_id) VALUES (pg_current_xact_id())",
                "INSERT INTO identity.avatars (transaction_id) VALUES (pg_current_xact_id())",
                "DELETE FROM identity.cancellations",
                "UPDATE identity.cancellations SET cancelled_at=cancelled_at",
                "UPDATE identity.registrations SET token=token",
                "UPDATE identity.messages SET worker_id = gen_random_uuid()",
                "UPDATE identity.checks SET hash = decode(repeat('00', 32), 'hex')",
                "UPDATE identity.confirmations SET person_id = gen_random_uuid()",
                "UPDATE identity.transfers SET session_id = gen_random_uuid()",
                "INSERT INTO identity.transfers (transaction_id) VALUES (pg_current_xact_id())",
                "UPDATE identity.rejections SET hash = decode(repeat('00', 32), 'hex')",
                "INSERT INTO identity.rejections (transaction_id) VALUES (pg_current_xact_id())",
                "UPDATE identity.replacements SET completed_at=completed_at",
                "UPDATE identity.claims SET worker_id = gen_random_uuid()",
                "SELECT setval('identity.ordinals', 1)",
            ] {
                let error = runtime.client().batch_execute(sql).await.expect_err("runtime permission denied");
                assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE));
            }
            for sql in [
                "INSERT INTO access.appointments DEFAULT VALUES",
                "INSERT INTO access.holders DEFAULT VALUES",
                "INSERT INTO identity.persons DEFAULT VALUES",
                "INSERT INTO identity.origins DEFAULT VALUES",
                "INSERT INTO identity.corrections DEFAULT VALUES",
                "INSERT INTO identity.cancellations (transaction_id) VALUES (pg_current_xact_id())",
                "INSERT INTO identity.sessions (transaction_id) VALUES (pg_current_xact_id())",
                "INSERT INTO identity.accounts (transaction_id) VALUES (pg_current_xact_id())",
                "INSERT INTO identity.replacements (transaction_id) VALUES (pg_current_xact_id())",
            ] {
                let error = runtime.client().batch_execute(sql).await.expect_err("authorized writes still require complete sealed facts");
                assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::NOT_NULL_VIOLATION));
            }
            let error = runtime.client().batch_execute("UPDATE identity.accounts SET id = id").await.expect_err("authorized state writes require a new paired record");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            assert_eq!(error.as_db_error().and_then(tokio_postgres::error::DbError::constraint), Some("accounts_records_check"));
            let error = runtime.client().batch_execute("DELETE FROM identity.challenges").await.expect_err("fresh history is protected despite DELETE permission");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            runtime.terminate().await.expect("end runtime");
            let owner = connections(&server, &name, "steward").recover().await.expect("owner");
            for (grant, revoke) in [
                ("REVOKE UPDATE (calls) ON identity.challenges FROM clerk", "GRANT UPDATE (calls) ON identity.challenges TO clerk"),
                ("REVOKE UPDATE (status) ON identity.challenges FROM clerk", "GRANT UPDATE (status) ON identity.challenges TO clerk"),
                ("REVOKE UPDATE (solved_at) ON identity.challenges FROM clerk", "GRANT UPDATE (solved_at) ON identity.challenges TO clerk"),
                ("REVOKE UPDATE (finished_at) ON identity.challenges FROM clerk", "GRANT UPDATE (finished_at) ON identity.challenges TO clerk"),
                ("GRANT UPDATE (source) ON identity.challenges TO clerk", "REVOKE UPDATE (source) ON identity.challenges FROM clerk"),
                ("GRANT MAINTAIN ON security.clocks TO clerk", "REVOKE MAINTAIN ON security.clocks FROM clerk"),
                ("GRANT UPDATE (active) ON security.keys TO clerk", "REVOKE UPDATE (active) ON security.keys FROM clerk"),
                ("GRANT INSERT (id) ON security.keys TO clerk", "REVOKE INSERT (id) ON security.keys FROM clerk"),
                ("GRANT MAINTAIN ON security.keys TO clerk", "REVOKE MAINTAIN ON security.keys FROM clerk"),
                ("REVOKE SELECT ON security.keys FROM clerk", "GRANT SELECT ON security.keys TO clerk"),
                ("REVOKE UPDATE (status) ON identity.messages FROM clerk", "GRANT UPDATE (status) ON identity.messages TO clerk"),
                ("REVOKE UPDATE (finished_at) ON identity.messages FROM clerk", "GRANT UPDATE (finished_at) ON identity.messages TO clerk"),
                ("GRANT UPDATE (worker_id) ON identity.messages TO clerk", "REVOKE UPDATE (worker_id) ON identity.messages FROM clerk"),
                ("GRANT UPDATE (hash) ON identity.checks TO clerk", "REVOKE UPDATE (hash) ON identity.checks FROM clerk"),
                ("REVOKE SELECT ON identity.confirmations FROM clerk", "GRANT SELECT ON identity.confirmations TO clerk"),
                ("REVOKE UPDATE (failures) ON identity.confirmations FROM clerk", "GRANT UPDATE (failures) ON identity.confirmations TO clerk"),
                ("GRANT UPDATE (hash) ON identity.confirmations TO clerk", "REVOKE UPDATE (hash) ON identity.confirmations FROM clerk"),
                ("REVOKE DELETE ON identity.transfers FROM clerk", "GRANT DELETE ON identity.transfers TO clerk"),
                ("GRANT UPDATE ON identity.transfers TO clerk", "REVOKE UPDATE ON identity.transfers FROM clerk"),
                ("REVOKE INSERT (hash) ON identity.rejections FROM clerk", "GRANT INSERT (hash) ON identity.rejections TO clerk"),
                ("GRANT INSERT (transaction_id) ON identity.rejections TO clerk", "REVOKE INSERT (transaction_id) ON identity.rejections FROM clerk"),
                ("REVOKE DELETE ON identity.challenges FROM clerk", "GRANT DELETE ON identity.challenges TO clerk"),
                ("REVOKE DELETE ON identity.messages FROM clerk", "GRANT DELETE ON identity.messages TO clerk"),
                ("REVOKE DELETE ON identity.checks FROM clerk", "GRANT DELETE ON identity.checks TO clerk"),
                ("REVOKE DELETE ON identity.claims FROM clerk", "GRANT DELETE ON identity.claims TO clerk"),
                ("REVOKE USAGE ON SEQUENCE identity.ordinals FROM clerk", "GRANT USAGE ON SEQUENCE identity.ordinals TO clerk"),
                ("GRANT SELECT ON SEQUENCE identity.ordinals TO clerk", "REVOKE SELECT ON SEQUENCE identity.ordinals FROM clerk"),
                ("GRANT UPDATE ON SEQUENCE identity.ordinals TO clerk", "REVOKE UPDATE ON SEQUENCE identity.ordinals FROM clerk"),
                ("GRANT DELETE ON security.keys TO clerk", "REVOKE DELETE ON security.keys FROM clerk"),
                ("REVOKE INSERT (id) ON identity.claims FROM clerk", "GRANT INSERT (id) ON identity.claims TO clerk"),
                ("GRANT INSERT ON identity.challenges TO clerk", "REVOKE INSERT ON identity.challenges FROM clerk; GRANT INSERT (id, operation_id, purpose, person_id, session_id, transfer_id, hash, key_id, mobile, source, calls, created_at, expires_at) ON identity.challenges TO clerk"),
                ("GRANT REFERENCES (mobile) ON identity.challenges TO clerk", "REVOKE REFERENCES (mobile) ON identity.challenges FROM clerk"),
                ("GRANT CREATE ON SCHEMA security TO clerk", "REVOKE CREATE ON SCHEMA security FROM clerk"),
                ("GRANT INSERT (transaction_id) ON identity.authentications TO clerk", "REVOKE INSERT (transaction_id) ON identity.authentications FROM clerk"),
                ("REVOKE INSERT ON identity.sessions FROM clerk", "GRANT INSERT ON identity.sessions TO clerk"),
                ("REVOKE UPDATE ON identity.accounts FROM clerk", "GRANT UPDATE ON identity.accounts TO clerk"),
                ("GRANT EXECUTE ON FUNCTION identity.lock_account(uuid) TO PUBLIC", "REVOKE EXECUTE ON FUNCTION identity.lock_account(uuid) FROM PUBLIC"),
                ("REVOKE EXECUTE ON FUNCTION identity.lock_account(uuid) FROM clerk", "GRANT EXECUTE ON FUNCTION identity.lock_account(uuid) TO clerk"),
                ("GRANT EXECUTE ON FUNCTION identity.lock_revocation(uuid,uuid,text) TO PUBLIC", "REVOKE EXECUTE ON FUNCTION identity.lock_revocation(uuid,uuid,text) FROM PUBLIC"),
                ("REVOKE EXECUTE ON FUNCTION identity.lock_revocation(uuid,uuid,text) FROM clerk", "GRANT EXECUTE ON FUNCTION identity.lock_revocation(uuid,uuid,text) TO clerk"),
                ("REVOKE EXECUTE ON FUNCTION identity.lock_registration(uuid) FROM clerk", "GRANT EXECUTE ON FUNCTION identity.lock_registration(uuid) TO clerk"),
                ("GRANT EXECUTE ON FUNCTION identity.lock_registration(uuid) TO PUBLIC", "REVOKE EXECUTE ON FUNCTION identity.lock_registration(uuid) FROM PUBLIC"),
                ("REVOKE SELECT ON identity.sessions FROM clerk", "GRANT SELECT ON identity.sessions TO clerk"),
                ("REVOKE DELETE ON identity.attempts FROM clerk", "GRANT DELETE ON identity.attempts TO clerk"),
                ("REVOKE DELETE ON identity.receipts FROM clerk", "GRANT DELETE ON identity.receipts TO clerk"),
                ("GRANT DELETE ON identity.accounts TO clerk", "REVOKE DELETE ON identity.accounts FROM clerk"),
                ("REVOKE DELETE ON identity.revocations FROM clerk", "GRANT DELETE ON identity.revocations TO clerk"),
                ("GRANT DELETE ON access.holders TO clerk", "REVOKE DELETE ON access.holders FROM clerk"),
                ("REVOKE UPDATE (released_at) ON identity.avatars FROM clerk", "GRANT UPDATE (released_at) ON identity.avatars TO clerk"),
                ("REVOKE EXECUTE ON FUNCTION identity.cancelling(uuid) FROM clerk", "GRANT EXECUTE ON FUNCTION identity.cancelling(uuid) TO clerk"),
                ("GRANT DELETE ON identity.cancellations TO clerk", "REVOKE DELETE ON identity.cancellations FROM clerk"),
            ] {
                owner.client().batch_execute(grant).await.expect("excess grant");
                let error = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(2, domain::identity::message::Limits::default(), session_settings(), domain::registration::Limits::default(), avatar_settings()).expect("settings"), process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                    supplier.client(), message().await).await.err().expect("excess privilege blocks start");
                assert!(matches!(error, process::Error::Database(domain::database::Error::Permissions)));
                owner.client().batch_execute(revoke).await.expect("remove excess grant");
            }
            let admin = connections(&server, &name, "postgres").recover().await.expect("admin");
            admin.client().batch_execute("CREATE SCHEMA auxiliary; REVOKE ALL ON SCHEMA auxiliary FROM PUBLIC").await.expect("unrelated schema");
            for (grant, revoke) in [
                ("GRANT CREATE ON SCHEMA auxiliary TO clerk", "REVOKE CREATE ON SCHEMA auxiliary FROM clerk"),
                ("GRANT CREATE ON SCHEMA auxiliary TO PUBLIC", "REVOKE CREATE ON SCHEMA auxiliary FROM PUBLIC"),
                ("ALTER SCHEMA auxiliary OWNER TO clerk", "ALTER SCHEMA auxiliary OWNER TO postgres; REVOKE ALL ON SCHEMA auxiliary FROM clerk"),
            ] {
                admin.client().batch_execute(grant).await.expect("excess permission in unrelated schema");
                let error = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(2, domain::identity::message::Limits::default(), session_settings(), domain::registration::Limits::default(), avatar_settings()).expect("settings"), process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                    supplier.client(), message().await).await.err().expect("every schema is checked");
                assert!(matches!(error, process::Error::Database(domain::database::Error::Permissions)));
                admin.client().batch_execute(revoke).await.expect("remove unrelated schema permission");
            }
            let role = format!("test_{}", Uuid::new_v4().simple());
            admin.client().batch_execute(&format!("CREATE ROLE {role}; GRANT {role} TO clerk WITH INHERIT FALSE")).await.expect("set-role-only membership");
            for (grant, revoke) in [
                (format!("GRANT UPDATE (source) ON identity.challenges TO {role}"), format!("REVOKE UPDATE (source) ON identity.challenges FROM {role}")),
                (format!("GRANT CREATE ON SCHEMA auxiliary TO {role}"), format!("REVOKE CREATE ON SCHEMA auxiliary FROM {role}")),
                (format!("ALTER SCHEMA auxiliary OWNER TO {role}"), format!("ALTER SCHEMA auxiliary OWNER TO postgres; REVOKE ALL ON SCHEMA auxiliary FROM {role}")),
            ] {
                admin.client().batch_execute(&grant).await.expect("set-role-only excess");
                let error = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(2, domain::identity::message::Limits::default(), session_settings(), domain::registration::Limits::default(), avatar_settings()).expect("settings"), process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                    supplier.client(), message().await).await.err().expect("SET ROLE excess blocks start");
                assert!(matches!(error, process::Error::Database(domain::database::Error::Permissions)));
                admin.client().batch_execute(&revoke).await.expect("remove role permission");
            }
            admin.client().batch_execute(&format!("REVOKE {role} FROM clerk")).await.expect("remove membership");
            admin.terminate().await.expect("end admin");
            owner.terminate().await.expect("end owner");
            let mut service = start(&server, &name, 0, 2, &supplier).await;
            service.stop().await.expect("repaired permissions start");
        });
    }

    #[test]
    fn isolated_process() {
        let Ok(name) = std::env::var("NUCLEUS_ACCEPTANCE_TEST_DATABASE") else {
            return;
        };
        let port = std::env::var("NUCLEUS_ACCEPTANCE_TEST_PORT")
            .expect("fixture port")
            .parse()
            .expect("port number");
        let authority =
            std::env::var("NUCLEUS_ACCEPTANCE_TEST_AUTHORITY").expect("fixture authority");
        let connections = Connections::new(
            database::Settings::new(
                "127.0.0.1".into(),
                port,
                name,
                "clerk".into(),
                2,
                database::settings::Budgets::default(),
            )
            .expect("fixture settings"),
            Password::new(fixture::PASSWORD.into()).expect("fictional password"),
            database::assembly::roots(
                &std::fs::read(authority).expect("generated test certificate"),
            )
            .expect("test trust anchor"),
        )
        .expect("connections");
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let service = Application::start(
                connections,
                keys(0),
                Settings::new(
                    2,
                    domain::identity::message::Limits::default(),
                    session_settings(),
                    domain::registration::Limits::default(),
                    avatar_settings(),
                )
                .expect("settings"),
                process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                supplier.client(),
                message().await,
            )
            .await
            .expect("verified start");
            service
                .authentication()
                .verify(input(OPERATION, "13800000000", "fictional-token"))
                .await
                .expect("original acceptance");
            // Exit after the durable result without an in-process stop or Rust destructors.
            std::process::exit(0);
        });
    }

    #[test]
    fn a_new_process_restores_the_same_acceptance_after_confirmed_old_process_exit() {
        let server = server();
        let runtime = Runtime::new().expect("runtime");
        let name = runtime.block_on(database(&server));
        let mut original = None;
        for _ in 0..2 {
            let mut child =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", "process::tests::isolated_process", "--nocapture"])
                    .env("NUCLEUS_ACCEPTANCE_TEST_DATABASE", &name)
                    .env("NUCLEUS_ACCEPTANCE_TEST_PORT", server.port().to_string())
                    .env("NUCLEUS_ACCEPTANCE_TEST_AUTHORITY", server.authority())
                    .spawn()
                    .expect("isolated application process");
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(status) = child.try_wait().expect("process status") {
                    assert!(status.success());
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    child.kill().expect("stop timed-out fixture");
                    child.wait().expect("reap fixture");
                    panic!("isolated process budget expired");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let stored = runtime.block_on(async {
                let read = connections(&server, &name, "clerk").recover().await.expect("read original");
                let rows = read.client().query("SELECT id::text, created_at::text, expires_at::text, calls FROM identity.challenges", &[]).await.expect("stored acceptance");
                assert_eq!(rows.len(), 1);
                let row = &rows[0];
                let stored = (row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, String>(2), row.get::<_, i16>(3));
                read.terminate().await.expect("end read");
                stored
            });
            match &original {
                Some(original) => assert_eq!(&stored, original),
                None => original = Some(stored),
            }
        }
    }
}
