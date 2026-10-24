//! Application-owned startup, shutdown and instance lifetime.

mod retention;
mod transport;
mod unit;

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
use platform::{database::Connections, process::Instance};
use thiserror::Error;
use tokio::{
    signal::unix::{SignalKind, signal},
    time::{self, timeout_at},
};

use crate::{identity, keys, settings, source, suppliers};

pub const DEFAULT_STOP_SECONDS: u64 = 30;

/// ExecStartPre checks only process membership, without loading credentials or opening a database.
pub fn check() -> Result<(), RunError> {
    Ok(unit::verify()?)
}

/// The production entrypoint has no fixture endpoints or alternate credential sources.
pub fn run() -> Result<(), RunError> {
    tracing::info!(stage = "start", "starting online service");
    check()?;
    if std::env::var_os("NOTIFY_SOCKET").is_none() {
        return Err(RunError::NotifyMissing);
    }
    let settings = settings::Settings::load()?;
    let instance = Instance::acquire(std::path::Path::new("/run/nucleus/instance"))?;
    let keys = authentication::Keys::new(
        keys::challenge::load()?,
        keys::mobile::load()?,
        keys::encryption::load()?,
        keys::message::load()?,
    );
    let connections = crate::database::load(settings.database)?;
    let (challenge, message) =
        suppliers::Clients::new(settings.challenge, settings.message)?.into_parts();
    let runtime = tokio::runtime::Runtime::new().map_err(RunError::Runtime)?;
    let stop_budget = settings.process.stop;
    runtime.block_on(supervise(
        Application::start(
            connections,
            keys,
            settings.authentication,
            settings.process,
            instance,
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
    requests: identity::authentication::Settings,
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
            // Keep the start future and instance lock alive until termination or exit.
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
    // Do not unwind the application or its instance lock before unfinished work exits.
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
    Unit(#[from] unit::Error),
    #[error("{0}")]
    Settings(#[from] settings::Error),
    #[error("{0}")]
    Instance(#[from] platform::process::Error),
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
            Self::Unit(_) => "isolation",
            Self::Settings(_) => "settings",
            Self::Instance(_) => "instance",
            Self::Keys(_) => "keys",
            Self::Database(_) => "database_assembly",
            Self::Suppliers(_) => "suppliers",
            Self::Application(Error::Database(_)) => "database_verification",
            Self::Application(Error::Authentication(_)) => "authentication",
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
            Self::NotifyMissing | Self::Notify(_) => "readiness",
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
    authentication: authentication::Service,
    settings: Settings,
    phase: Phase,
    // Timeout reports share completed failures with subsequent stop attempts.
    failures: Vec<Arc<Error>>,
    // Keep the process lock even when the stop future is cancelled or times out.
    _instance: Instance,
}

impl Application {
    async fn serve(
        &mut self,
        address: std::net::SocketAddr,
        source: source::Settings,
        settings: identity::authentication::Settings,
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
            result = transport::Transport::bind(address, self.authentication.clone(), settings, source) => result,
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
            retention: retention::Retention::start(self.authentication.clone()),
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

    /// The caller proves the previous process exited and supplies the fixed instance lock.
    pub async fn start(
        connections: Connections,
        keys: authentication::Keys,
        authentication: authentication::Settings,
        settings: Settings,
        instance: Instance,
        supplier: client::Client,
        message: message::client::Client,
    ) -> Result<Self, Error> {
        let deadline = time::Instant::now() + Duration::from_secs(10);
        let context = runtime::Context::new(connections).await?;
        let cause = match async {
            let barrier = timeout_at(deadline, Barrier::begin(context.clone())).await??;
            Ok::<_, Error>(
                authentication::Service::start(barrier, keys, authentication, supplier, message)
                    .await?,
            )
        }
        .await
        {
            Ok(authentication) => {
                return Ok(Self {
                    context,
                    authentication,
                    settings,
                    phase: Phase::Prepared,
                    failures: Vec::new(),
                    _instance: instance,
                });
            }
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
        &self.authentication
    }

    /// Drain without releasing the instance lock; dropping the application releases it.
    /// On timeout the caller must exit instead of reporting a successful drain.
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
        self.authentication.close();
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
            if let Err(error) = self.authentication.drain().await {
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
    Authentication(#[from] authentication::Error),
    #[error("{0}")]
    Runtime(#[from] runtime::Error),
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
    use domain::identity::{
        authentication::{Error, Input, Keys, Settings},
        challenge::{self, client},
        keys::{self, Entry, Material, metadata::Metadata},
    };
    use nonempty::NonEmpty;
    use platform::{
        database::{self, Connections, credentials::Password},
        process::Instance,
    };
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

    use crate::process::{self, Application};

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
            access_key::{AccessKey, Id, Secret},
            client::Client,
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

    struct Supplier {
        port: u16,
        task: JoinHandle<()>,
        calls: Arc<AtomicUsize>,
    }

    impl Supplier {
        async fn new() -> Self {
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

    fn server() -> Arc<fixture::Server> {
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

    fn materials(
        active: i64,
    ) -> (
        keys::challenge::Keys,
        keys::mobile::Keys,
        keys::encryption::Keys,
        keys::message::Keys,
    ) {
        fn entries(offset: u8) -> Vec<Entry> {
            (0..2)
                .map(|id| {
                    Entry::new(
                        keys::Id::new(id).expect("key id"),
                        Material::new(Zeroizing::new(
                            URL_SAFE_NO_PAD
                                .encode([offset + u8::try_from(id).expect("key byte"); 32]),
                        ))
                        .expect("fictional material"),
                    )
                })
                .collect()
        }
        (
            keys::challenge::Keys::new(keys::Id::new(active).expect("active"), entries(1))
                .expect("challenge keys"),
            keys::mobile::Keys::new(keys::Id::new(0).expect("active"), entries(3))
                .expect("mobile keys"),
            keys::encryption::Keys::new(keys::Id::new(0).expect("active"), entries(5))
                .expect("encryption keys"),
            keys::message::Keys::new(keys::Id::new(0).expect("active"), entries(7))
                .expect("message keys"),
        )
    }

    fn keys(active: i64) -> Keys {
        let (challenge, mobile, encryption, message) = materials(active);
        Keys::new(challenge, mobile, encryption, message)
    }

    async fn database(server: &fixture::Server) -> String {
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
        transaction
            .batch_execute(include_str!("../../deployment/migrations/V1__keys.sql"))
            .await
            .expect("key migration");
        transaction
            .batch_execute(include_str!(
                "../../deployment/migrations/V2__challenge_acceptance.sql"
            ))
            .await
            .expect("challenge migration");
        transaction
            .batch_execute(include_str!(
                "../../deployment/migrations/V3__message_sending.sql"
            ))
            .await
            .expect("message migration");
        transaction
            .batch_execute(include_str!(
                "../../deployment/migrations/V4__retention.sql"
            ))
            .await
            .expect("retention migration");
        let (challenge, mobile, encryption, message) = materials(0);
        keys::database::initialize(
            &Metadata::new(&challenge, &mobile, &encryption, &message),
            &transaction,
        )
        .await
        .expect("key initialization");
        transaction.commit().await.expect("commit migrations");
        session.terminate().await.expect("end deployment");
        name
    }

    async fn start(
        server: &fixture::Server,
        name: &str,
        directory: &tempfile::TempDir,
        active: i64,
        capacity: usize,
        supplier: &Supplier,
    ) -> Application {
        Application::start(
            connections(server, name, "clerk"),
            keys(active),
            Settings::new(capacity, domain::identity::message::Limits::default())
                .expect("settings"),
            process::Settings::new(Duration::from_secs(30)).expect("process settings"),
            Instance::acquire(&directory.path().join("instance")).expect("instance"),
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
        let settings = crate::identity::authentication::Settings::new("https://example.com".into())
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

    #[test]
    fn http_disconnect_and_shutdown_preserve_the_single_send_and_original_result() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let directory = tempfile::tempdir().expect("instance directory");
            let name = database(&server).await;
            let reader = connections(&server,&name,"clerk").recover().await.expect("trusted fixture time");
            let now: chrono::DateTime<chrono::Utc> = reader.client().query_one("SELECT clock_timestamp()", &[]).await.expect("time").get(0);
            reader.terminate().await.expect("reader ends");
            let supplier = Supplier::responding(serde_json::json!({"success":true,"challenge_ts":now,"hostname":"example.com","action":"send_authentication_code","cdata":OPERATION}).to_string()).await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server,&name,"clerk"), keys(0), Settings::new(8, domain::identity::message::Limits::default()).expect("settings"), process::Settings::new(Duration::from_secs(5)).expect("stop"), Instance::acquire(&directory.path().join("instance")).expect("lock"), supplier.client(), message_at(sms.address(),sms.authority())).await.expect("start");
            let (base,stop,mut task) = running(application).await;
            let client = http_client();
            let request = client.post(format!("{base}/codes")).header("operation-id",OPERATION).header("x-real-ip","::ffff:192.0.2.1").json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"}));
            let caller = tokio::spawn(request.send());
            let sent = timeout(Duration::from_secs(4),sms.request()).await.expect("single PNVS call");
            caller.abort();
            assert!(caller.await.expect_err("waiter cancelled").is_cancelled());
            stop.send(()).expect("stop with accepted send");
            assert!(timeout(Duration::from_millis(30),&mut task).await.is_err(), "shutdown must wait for the supplier conclusion");
            accept_sms(sent);
            let (application,result) = timeout(Duration::from_secs(5),task).await.expect("stop deadline").expect("server task");
            result.expect("drained original send");
            assert!(Instance::acquire(&directory.path().join("instance")).is_err());
            drop(application);
            let application = Application::start(connections(&server,&name,"clerk"), keys(0), Settings::new(8, domain::identity::message::Limits::default()).expect("settings"), process::Settings::new(Duration::from_secs(5)).expect("stop"), Instance::acquire(&directory.path().join("instance")).expect("restart lock"), supplier.client(), message_at(sms.address(),sms.authority())).await.expect("restart");
            let (base,stop,task) = running(application).await;
            let replay = |source: &'static str, token: &'static str, mobile: &'static str| client.post(format!("{base}/codes")).header("operation-id",OPERATION).header("x-real-ip",source).json(&serde_json::json!({"mobile":mobile,"token":token}));
            let response = replay("192.0.2.99","fictional-token","13800000000").send().await.expect("original recovery");
            assert_eq!(response.status(),200);
            let original: serde_json::Value = response.json().await.expect("original message");
            assert!(original.get("operationId").is_none());
            assert!(original.get("operation_id").is_none());
            assert_eq!(original.as_object().expect("response object").len(), 2);
            let message = &original["message"];
            assert_eq!(message.as_object().expect("message object").len(), 3);
            assert!(message["id"].as_str().expect("message UUID").parse::<domain::identity::message::Id>().is_ok());
            assert!(chrono::DateTime::parse_from_rfc3339(message["expiresAt"].as_str().expect("message expiry")).is_ok());
            assert!(message.get("expires_at").is_none());
            assert_eq!(original["message"]["delivery"],"sent");
            let response = replay("192.0.2.100","fictional-token","13800000000").send().await.expect("repeat recovery");
            assert_eq!(response.json::<serde_json::Value>().await.expect("same message"),original);
            assert_eq!(replay("192.0.2.101","wrong-token","13800000000").send().await.expect("wrong proof").status(),401);
            assert_eq!(replay("192.0.2.102","fictional-token","13900000000").send().await.expect("wrong mobile").status(),409);
            assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst),1);
            assert!(timeout(Duration::from_millis(50),sms.request()).await.is_err(),"recovery must not call PNVS");
            let reader = connections(&server,&name,"clerk").recover().await.expect("facts");
            let row = reader.client().query_one("SELECT (SELECT count(*) FROM identity.messages), (SELECT count(*) FROM identity.checks), (SELECT count(*) FROM identity.claims)",&[]).await.expect("pair and claim");
            assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1),row.get::<_,i64>(2)),(1,1,1));
            let row = reader.client().query_one("SELECT source::text FROM identity.challenges", &[]).await.expect("original source");
            assert_eq!(row.get::<_, String>(0), "192.0.2.1/32", "the proxy address and later recovery sources must not replace the original source");
            reader.terminate().await.expect("reader ends");
            stop.send(()).expect("stop replay process");
            let (application,result) = task.await.expect("server task");
            result.expect("drained");
            drop(application);
        });
    }

    #[test]
    fn background_retention_expires_identity_and_rechecks_late_requests() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let directory = tempfile::tempdir().expect("instance directory");
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let mut application = start(&server, &name, &directory, 0, 8, &supplier).await;
            application.authentication().verify(input(OPERATION, "13800000000", "fictional-token"))
                .await.expect("retained verification result");
            application.stop().await.expect("original process drained");
            drop(application);
            let admin = connections(&server, &name, "postgres").recover().await.expect("isolated fixture admin");
            // Advance the fictional record's age while preserving its original proof and time relationships.
            admin.client().batch_execute("BEGIN; ALTER TABLE identity.challenges DISABLE TRIGGER USER;
                UPDATE identity.challenges SET created_at=created_at-interval '25 hours', expires_at=expires_at-interval '25 hours', finished_at=finished_at-interval '25 hours';
                ALTER TABLE identity.challenges ENABLE TRIGGER USER; COMMIT").await.expect("expired history fixture");
            let application = start(&server, &name, &directory, 0, 8, &supplier).await;
            let (base, stop, task) = running(application).await;
            timeout(Duration::from_secs(5), async {
                loop {
                    let deleted: bool = admin.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM identity.challenges)", &[]).await.expect("deletion observer").get(0);
                    if deleted { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("process triggers retention");
            let response = http_client().post(format!("{base}/codes")).header("operation-id", OPERATION)
                .json(&serde_json::json!({"mobile":"13800000000", "token":"fictional-token"}))
                .send().await.expect("expired operation response");
            assert_eq!(response.status(), 403);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("operation-id"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("response JSON"), serde_json::json!({"code":"verification_invalid"}));
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
            let messages: i64 = admin.client().query_one("SELECT count(*) FROM identity.messages", &[]).await.expect("no send permission").get(0);
            assert_eq!(messages, 0);
            stop.send(()).expect("stop");
            let (application, result) = task.await.expect("process task");
            result.expect("clean stop");
            drop(application);
            admin.terminate().await.expect("fixture admin ended");
        });
    }

    #[test]
    fn stop_retains_running_retention_and_finishes_only_its_current_candidate() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let directory = tempfile::tempdir().expect("instance directory");
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let mut application = start(&server, &name, &directory, 0, 8, &supplier).await;
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
                let mut application = start(&server, &name, &directory, 0, 8, &supplier).await;
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
                assert!(Instance::acquire(&directory.path().join("instance")).is_err());
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
    fn http_validates_requests_before_business_work_and_describes_actual_routes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let directory = tempfile::tempdir().expect("instance directory");
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let application = start(&server, &name, &directory, 0, 8, &supplier).await;
            let (base, stop, task) = running(application).await;
            let client = http_client();
            for path in ["/health/live", "/health/ready", "/openapi.json"] {
                let operation_id = if path == "/openapi.json" { "invalid".to_owned() } else { OPERATION.to_uppercase() };
                let response = client.get(format!("{base}{path}")).header("operation-id", &operation_id).send().await.expect("technical endpoint");
                assert_eq!(response.status(), 200);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                let value: serde_json::Value = response.json().await.expect("JSON");
                if path == "/openapi.json" {
                    assert!(value.get("operationId").is_none());
                    assert_eq!(value["openapi"], "3.1.0");
                    let paths = value["paths"].as_object().expect("paths");
                    assert_eq!(paths.len(), 4);
                    for route in ["/codes", "/health/live", "/health/ready", "/openapi.json"] { assert!(paths.contains_key(route)); }
                    assert!(value["paths"]["/codes"]["post"]["responses"]["429"]["headers"]["Retry-After"].is_object());
                    for status in ["400", "413", "415", "422", "500"] {
                        assert!(value["paths"]["/codes"]["post"]["responses"][status].is_object());
                    }
                    let origin = value["paths"]["/codes"]["post"]["parameters"].as_array().expect("C1 parameters").iter().find(|parameter| parameter["name"] == "Origin").expect("origin");
                    assert_eq!(origin["required"], true);
                    let operation = value["paths"]["/codes"]["post"]["parameters"].as_array().expect("C1 parameters").iter().find(|parameter| parameter["name"] == "Operation-Id").expect("operation ID");
                    assert_eq!(operation["in"], "header");
                    assert_eq!(operation["required"], true);
                    assert_eq!(operation["schema"]["type"], "string");
                    assert_eq!(operation["schema"]["format"], "uuid");
                    let schemas = &value["components"]["schemas"];
                    let failure = &schemas["Failure"];
                    assert_eq!(failure["properties"].as_object().expect("error fields").len(), 1);
                    assert_eq!(failure["required"], serde_json::json!(["code"]));
                    assert_eq!(failure["properties"]["code"]["type"], "string");
                    assert!(failure["properties"]["code"].get("enum").is_none());
                    assert!(failure["properties"]["code"].get("$ref").is_none());
                    for name in ["Failure", "Receipt", "Live", "Ready"] {
                        let fields = &schemas[name]["properties"];
                        assert!(fields.get("operationId").is_none());
                        assert!(fields.get("operation_id").is_none());
                    }
                    let fields = &schemas["Message"]["properties"];
                    assert_eq!(fields["id"]["$ref"], "#/components/schemas/message.Id");
                    assert_eq!(schemas["message.Id"]["type"], "string");
                    assert_eq!(schemas["message.Id"]["format"], "uuid");
                    assert_eq!(fields["expiresAt"]["format"], "date-time");
                    assert!(fields.get("expires_at").is_none());
                    assert!(value["paths"]["/health/live"]["head"].is_object());
                    assert!(value["paths"]["/health/live"]["head"]["responses"]["200"].get("content").is_none());
                    for path in ["/health/live", "/health/ready"] {
                        for method in ["get", "head"] {
                            let operation = &value["paths"][path][method];
                            let parameter = operation["parameters"].as_array().expect("health parameters").iter().find(|parameter| parameter["name"] == "Operation-Id").expect("required request identity");
                            assert_eq!(parameter["in"], "header");
                            assert_eq!(parameter["required"], true);
                            assert_eq!(parameter["schema"]["type"], "string");
                            assert_eq!(parameter["schema"]["format"], "uuid");
                            assert!(operation["responses"]["400"].is_object());
                            if method == "head" {
                                assert!(operation["responses"]["400"].get("content").is_none());
                            }
                        }
                    }
                } else {
                    let field = if path == "/health/live" { "live" } else { "ready" };
                    assert_eq!(value, serde_json::json!({"code":"ok",field:true}));
                }
                let request = client.head(format!("{base}{path}"));
                let request = if path == "/openapi.json" { request } else { request.header("operation-id", &operation_id) };
                let response = request.send().await.expect("HEAD");
                assert_eq!(response.status(), 200);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                assert!(response.bytes().await.expect("empty body").is_empty());
            }
            for path in ["/health/live", "/health/ready"] {
                for method in [reqwest::Method::GET, reqwest::Method::HEAD, reqwest::Method::POST] {
                    for headers in [vec![], vec![""], vec!["invalid"], vec![OPERATION, OPERATION], vec![OPERATION, OTHER]] {
                        let mut request = client.request(method.clone(), format!("{base}{path}"));
                        for header in headers { request = request.header("operation-id", header); }
                        let response = request.send().await.expect("invalid health identifier");
                        assert_eq!(response.status(), 400);
                        assert_eq!(response.headers()["cache-control"], "no-store");
                        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                        if method == reqwest::Method::HEAD {
                            assert!(response.bytes().await.expect("empty HEAD rejection").is_empty());
                        } else {
                            assert_eq!(response.json::<serde_json::Value>().await.expect("health rejection"), serde_json::json!({"code":"invalid_input"}));
                        }
                    }
                }
                let response = client.post(format!("{base}{path}")).header("operation-id", OPERATION).send().await.expect("health method refusal");
                assert_eq!(response.status(), 405);
                assert!(response.headers()["allow"].to_str().expect("allowed methods").contains("GET"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("health method JSON"), serde_json::json!({"code":"method_not_allowed"}));
            }
            for (path, expected, code) in [("/codes", 405, "method_not_allowed"), ("/sessions", 404, "resource_not_found")] {
                let response = client.get(format!("{base}{path}")).header("operation-id", OPERATION).header("x-real-ip", "invalid").header("origin", "https://attacker.invalid").send().await.expect("route refusal");
                assert_eq!(response.status(), expected);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                let value: serde_json::Value = response.json().await.expect("safe JSON");
                assert_eq!(value["code"], code);
                assert_eq!(value, serde_json::json!({"code":code}));
            }
            for path in ["/sessions", "/codes", "/openapi.json"] {
                let response = client.post(format!("{base}{path}")).send().await.expect("fallback identifier");
                assert_eq!(response.status(), 400);
                assert_eq!(response.json::<serde_json::Value>().await.expect("fallback error"), serde_json::json!({"code":"invalid_input"}));
                let response = client.head(format!("{base}{path}")).header("operation-id", OPERATION).send().await.expect("fallback HEAD");
                assert_eq!(response.status(), if path == "/sessions" {404} else if path == "/codes" {405} else {200});
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                assert!(response.bytes().await.expect("HEAD body").is_empty());
            }
            let plain = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client without Origin");
            assert_eq!(plain.get(format!("{base}/health/live")).header("operation-id", OPERATION).send().await.expect("health needs no Origin").status(), 200);
            for sources in [vec![], vec![""], vec!["invalid"], vec!["192.0.2.1", "192.0.2.1"], vec!["192.0.2.1, 192.0.2.2"], vec!["192.0.2.1:80"]] {
                let mut request = plain.post(format!("{base}/codes"))
                    .header("operation-id", OPERATION)
                    .header("origin", "https://example.com")
                    .header("x-forwarded-for", "192.0.2.1")
                    .body("unread body");
                for source in sources { request = request.header("x-real-ip", source); }
                let response = request.send().await.expect("source refusal");
                assert_eq!(response.status(), 400);
                assert_eq!(response.json::<serde_json::Value>().await.expect("source error"), serde_json::json!({"code":"invalid_input"}));
            }
            for (index, origins) in [vec![], vec![""], vec!["null"], vec!["https://example.com", "https://example.com"], vec!["https://attacker.invalid"]].into_iter().enumerate() {
                let mut request = plain.post(format!("{base}/codes"))
                    .header("operation-id", OTHER)
                    .header("authorization", "Bearer fictional-ignored-token")
                    .header("x-real-ip", format!("198.51.100.{}", index + 10))
                    .body("unread body");
                for origin in origins { request = request.header("origin", origin); }
                let response = request.send().await.expect("Origin rejection");
                assert_eq!(response.status(), 403);
                assert_eq!(response.json::<serde_json::Value>().await.expect("Origin error"), serde_json::json!({"code":"permission_denied"}));
            }
            for (index, (types, expected)) in [(vec![], 415), (vec!["text/plain"], 415), (vec!["application/problem+json"], 422), (vec!["application/json", "application/json"], 422)].into_iter().enumerate() {
                let mut request = client.post(format!("{base}/codes")).header("operation-id", OTHER).header("x-real-ip", format!("198.51.100.{}", index + 30)).body("{}");
                for content in types { request = request.header("content-type", content); }
                let response = request.send().await.expect("media and field rejection");
                assert_eq!(response.status(), expected);
                assert_eq!(response.json::<serde_json::Value>().await.expect("media error"), serde_json::json!({"code":"invalid_input"}));
            }
            for headers in [vec![], vec![""], vec!["invalid"], vec![OPERATION, OTHER], vec!["00000000-0000-0000-0000-000000000000, ffffffff-ffff-ffff-ffff-ffffffffffff"]] {
                let mut request = client.post(format!("{base}/codes")).body("not JSON");
                for header in headers { request = request.header("operation-id", header); }
                let response = request.send().await.expect("invalid identifier");
                assert_eq!(response.status(), 400);
                let value: serde_json::Value = response.json().await.expect("error JSON");
                assert_eq!(value["code"], "invalid_input");
                assert_eq!(value, serde_json::json!({"code":"invalid_input"}));
            }
            for (index, (body, expected)) in [
                (r#"{}"#.to_string(), 422),
                (r#"{"mobile":"13800000000","token":"a","token":"b"}"#.into(), 422),
                (r#"{"mobile":"13800000000","token":"a","extra":true}"#.into(), 422),
                (r#"{"mobile":13800000000,"token":"a"}"#.into(), 422),
                (r#"{"mobile":"+8613800000000","token":"a"}"#.into(), 422),
                (r#"{"mobile":"13800000000","token":null}"#.into(), 422),
                (format!(r#"{{"mobile":"13800000000","token":"{}"}}"#, "a".repeat(2049)), 422),
                ("{".into(), 400),
                (r#"{"mobile":"13800000000","token":"a"} {}"#.into(), 400),
                (" ".repeat(8193), 413),
            ].into_iter().enumerate() {
                let response = client.post(format!("{base}/codes")).header("operation-id", OTHER.to_uppercase()).header("x-real-ip", format!("192.0.2.{}",index+1)).header("content-type", "application/json").body(body).send().await.expect("invalid body");
                assert_eq!(response.status(), expected);
                let value: serde_json::Value = response.json().await.expect("safe response");
                assert_eq!(value, serde_json::json!({"code":"invalid_input"}));
            }
            for (source, origin, content, expected) in [("invalid", "https://example.com", "application/json",400), ("192.0.2.100", "https://attacker.invalid", "application/json",403), ("192.0.2.101", "https://example.com", "text/plain",415)] {
                let response = client.post(format!("{base}/codes")).header("operation-id", OTHER).header("x-real-ip",source).header("origin",origin).header("content-type",content).body("{}").send().await.expect("boundary refusal");
                assert_eq!(response.status(),expected);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                let code = if expected == 403 { "permission_denied" } else { "invalid_input" };
                assert_eq!(response.json::<serde_json::Value>().await.expect("boundary error"), serde_json::json!({"code":code}));
            }
            for expected in [422,422,429] {
                let response = client.post(format!("{base}/codes")).header("operation-id",OTHER).header("x-real-ip","198.51.100.1").header("content-type","application/json").body("{}").send().await.expect("ingress quota");
                assert_eq!(response.status(),expected);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                if expected == 429 { assert!(response.headers()["retry-after"].to_str().expect("wait").parse::<u64>().expect("seconds") >= 1); }
                let code = if expected == 429 { "rate_limited" } else { "invalid_input" };
                assert_eq!(response.json::<serde_json::Value>().await.expect("quota error"), serde_json::json!({"code":code}));
            }
            assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst),0);
            for body in [serde_json::json!({"mobile":"13800000000","token":"fictional-token"}), serde_json::json!(["13800000000", "fictional-token"])] {
                let response = client.post(format!("{base}/codes")).header("operation-id",OPERATION).header("x-real-ip","203.0.113.1").json(&body).send().await.expect("C1 rejection");
                assert_eq!(response.status(),403);
                assert_eq!(response.json::<serde_json::Value>().await.expect("rejection")["code"],"verification_invalid");
            }
            assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst),1);
            let owner = connections(&server,&name,"steward").recover().await.expect("fixture owner");
            owner.client().batch_execute("UPDATE security.clocks SET observed_at = clock_timestamp() + interval '1 hour'").await.expect("time rollback fixture");
            let response = client.get(format!("{base}/health/ready")).header("operation-id", OPERATION).send().await.expect("readiness");
            assert_eq!(response.status(),503);
            assert_eq!(response.json::<serde_json::Value>().await.expect("not ready"),serde_json::json!({"code":"dependency_unavailable"}));
            let response = client.head(format!("{base}/health/ready")).header("operation-id", OPERATION).send().await.expect("HEAD readiness");
            assert_eq!(response.status(),503);
            assert!(response.bytes().await.expect("empty readiness body").is_empty());
            assert_eq!(client.get(format!("{base}/health/live")).header("operation-id", OPERATION).send().await.expect("liveness").status(),200);
            owner.terminate().await.expect("owner ends");
            stop.send(()).expect("stop");
            let (application,result) = timeout(Duration::from_secs(5),task).await.expect("stop budget").expect("HTTP task");
            result.expect("drained");
            drop(application);
        });
    }

    #[test]
    fn http_restart_resets_ingress_but_preserves_durable_message_quota() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let directory = tempfile::tempdir().expect("instance directory");
            let name = database(&server).await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let client = http_client();
            let mut original = serde_json::Value::Null;
            for operation_id in [OPERATION, OTHER] {
                let reader = connections(&server, &name, "clerk").recover().await.expect("fixture time");
                let now: chrono::DateTime<chrono::Utc> = reader.client().query_one("SELECT clock_timestamp()", &[]).await.expect("time").get(0);
                reader.terminate().await.expect("reader ends");
                let supplier = Supplier::responding(serde_json::json!({"success":true,"challenge_ts":now,"hostname":"example.com","action":"send_authentication_code","cdata":operation_id}).to_string()).await;
                let application = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(8, domain::identity::message::Limits::default()).expect("settings"), process::Settings::new(Duration::from_secs(5)).expect("stop"), Instance::acquire(&directory.path().join("instance")).expect("lock"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("start");
                let (base, stop, task) = running(application).await;
                let request = client.post(format!("{base}/codes")).header("operation-id", operation_id).json(&serde_json::json!({"mobile":"13800000000","token":format!("fictional-{operation_id}")}));
                let response = if operation_id == OPERATION {
                    let caller = tokio::spawn(request.send());
                    accept_sms(timeout(Duration::from_secs(4), sms.request()).await.expect("first send"));
                    caller.await.expect("caller").expect("HTTP response")
                } else {
                    request.send().await.expect("durable quota response")
                };
                if operation_id == OPERATION {
                    assert_eq!(response.status(), 200);
                    original = response.json().await.expect("original message");
                } else {
                    assert_eq!(response.status(), 429);
                    let seconds: u64 = response.headers()["retry-after"].to_str().expect("seconds").parse().expect("integer");
                    assert!((1..=60).contains(&seconds));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("quota JSON"), serde_json::json!({"code":"rate_limited"}));
                    let replay = client.post(format!("{base}/codes")).header("operation-id", OPERATION).json(&serde_json::json!({"mobile":"13800000000","token":format!("fictional-{OPERATION}")})).send().await.expect("original replay");
                    assert_eq!(replay.status(), 200);
                    assert_eq!(replay.json::<serde_json::Value>().await.expect("same message"), original);
                    assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err());
                }
                assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
                stop.send(()).expect("stop");
                let (application, result) = task.await.expect("HTTP task");
                result.expect("drained");
                drop(application);
            }
        });
    }

    #[test]
    fn a_pending_stop_prevents_readiness_and_preserves_the_lock_until_drop() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let directory = tempfile::tempdir().expect("instance directory");
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let mut application = start(&server, &name, &directory, 0, 8, &supplier).await;
            application
                .serve(
                    "127.0.0.1:0".parse().expect("listener"),
                    crate::source::Settings::new(vec!["127.0.0.1/32".parse().expect("proxy")])
                        .expect("source"),
                    crate::identity::authentication::Settings::new("https://example.com".into())
                        .expect("origin"),
                    std::future::ready(Ok(())),
                    |_| panic!("stopped process must never announce readiness"),
                )
                .await
                .expect("drains without serving");
            assert!(application.authentication().ready().await.is_err());
            assert!(Instance::acquire(&directory.path().join("instance")).is_err());
            drop(application);
            assert!(Instance::acquire(&directory.path().join("instance")).is_ok());
        });
    }

    #[test]
    fn a_completed_retention_failure_prevents_readiness_and_drains_the_application() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(10), async {
                let directory = tempfile::tempdir().expect("instance directory");
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let mut application = start(&server, &name, &directory, 0, 8, &supplier).await;
                let transport = process::transport::Transport::bind(
                    "127.0.0.1:0".parse().expect("listener"),
                    application.authentication.clone(),
                    crate::identity::authentication::Settings::new("https://example.com".into())
                        .expect("origin"),
                    crate::source::Settings::new(vec!["127.0.0.1/32".parse().expect("proxy")])
                        .expect("source"),
                )
                .await
                .expect("HTTP transport");
                let admin = connections(&server, &name, "postgres")
                    .recover()
                    .await
                    .expect("fixture admin");
                admin.client().batch_execute(
                    "UPDATE security.clocks SET observed_at = clock_timestamp() + interval '1 hour'",
                ).await.expect("clock rollback after startup validation");
                let retention = process::retention::Retention::start(application.authentication.clone());
                // Establish the failed-task boundary before polling the readiness callback.
                while !retention.finished() {
                    tokio::task::yield_now().await;
                }
                application.phase = process::Phase::Running { transport, retention };
                let mut announced = false;
                let result = application.phase.wait(std::future::pending(), |_| {
                    announced = true;
                    Ok(())
                }).await;
                let error = application.finish(
                    result,
                    tokio::time::Instant::now() + Duration::from_secs(3),
                ).await.expect_err("failed retention prevents startup");
                assert!(!announced, "a failed application must not publish readiness");
                assert!(matches!(error, process::RunError::Retention(process::retention::Error::Operation(
                    Error::Runtime(domain::runtime::Error::ClockInvalid)
                ))));
                assert!(matches!(application.phase, process::Phase::Stopped));
                assert!(application.authentication().ready().await.is_err());
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 0);
                assert!(Instance::acquire(&directory.path().join("instance")).is_err());
                drop(application);
                assert!(Instance::acquire(&directory.path().join("instance")).is_ok());
                admin.terminate().await.expect("fixture admin ended");
            }).await.expect("startup retention failure budget");
        });
    }

    #[test]
    fn http_start_failures_close_admission_and_keep_the_instance_owned() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            for occupied in [true, false] {
                let directory = tempfile::tempdir().expect("instance directory");
                let name = database(&server).await;
                let mut application = start(&server, &name, &directory, 0, 8, &supplier).await;
                let listener = TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("occupied listener");
                let address = if occupied {
                    listener.local_addr().expect("address")
                } else {
                    "127.0.0.1:0".parse().expect("dynamic port")
                };
                let settings =
                    crate::identity::authentication::Settings::new("https://example.com".into())
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
                assert!(application.authentication().ready().await.is_err());
                assert!(Instance::acquire(&directory.path().join("instance")).is_err());
                drop(application);
                assert!(Instance::acquire(&directory.path().join("instance")).is_ok());
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
                let directory = tempfile::tempdir().expect("instance directory");
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let mut application = start(&server, &name, &directory, 0, 8, &supplier).await;
                application.settings.stop = Duration::from_millis(300);
                let admin = connections(&server, &name, "postgres").recover().await.expect("fixture admin");
                admin.client().batch_execute("BEGIN; LOCK TABLE security.clocks IN ACCESS EXCLUSIVE MODE")
                    .await.expect("hold the first retention sample");
                let (ready, address) = tokio::sync::oneshot::channel();
                let (stop, stopped) = tokio::sync::oneshot::channel();
                let (noticed, notice) = tokio::sync::oneshot::channel();
                let task = tokio::spawn(async move {
                    let result = application.serve(
                        "127.0.0.1:0".parse().expect("listener"),
                        crate::source::Settings::new(vec!["127.0.0.1/32".parse().expect("proxy")]).expect("source"),
                        crate::identity::authentication::Settings::new("https://example.com".into()).expect("origin"),
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
                wait_for_lock(admin.client()).await;

                // 100 Continue proves the handler has started waiting for this body's bytes.
                let mut reader = TcpStream::connect(address)
                    .await.expect("slow HTTP reader");
                reader.write_all(format!(
                    "POST /codes HTTP/1.1\r\nHost: localhost\r\nOperation-Id: {OPERATION}\r\nOrigin: https://example.com\r\nX-Real-IP: 192.0.2.1\r\nContent-Type: application/json\r\nContent-Length: 80\r\nExpect: 100-continue\r\n\r\n"
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
                admin.client().batch_execute("UPDATE security.clocks SET observed_at = clock_timestamp() + interval '1 hour'; COMMIT")
                    .await.expect("release the sample with a future stored time");
                let (mut application, result) = timeout(Duration::from_secs(2), task).await
                    .expect("bounded failed shutdown").expect("process task");
                let error = result.expect_err("retention failure and HTTP timeout");
                if stopping {
                    assert!(matches!(&error, process::RunError::Application(process::Error::Timeout { failures, .. })
                        if matches!(failures.as_slice(), [failure]
                            if matches!(failure.as_ref(), process::Error::Retention(process::retention::Error::Operation(
                                Error::Runtime(domain::runtime::Error::ClockInvalid)))))));
                } else {
                    assert!(matches!(&error, process::RunError::Combined { cause, termination: process::Error::Timeout { .. } }
                    if matches!(cause.as_ref(), process::RunError::Retention(process::retention::Error::Operation(
                        Error::Runtime(domain::runtime::Error::ClockInvalid))))));
                }
                assert!(error.incomplete());
                let diagnostic = format!("{error:?}");
                assert!(diagnostic.contains("ClockInvalid"));
                assert!(diagnostic.contains("Timeout"));
                assert!(!diagnostic.contains("RetentionEnded"));
                assert!(Instance::acquire(&directory.path().join("instance")).is_err());
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
                assert!(Instance::acquire(&directory.path().join("instance")).is_err());
                drop(application);
                assert!(Instance::acquire(&directory.path().join("instance")).is_ok());
                admin.terminate().await.expect("fixture admin ended");
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
                let directory = tempfile::tempdir().expect("instance directory");
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let mut application = start(&server, &name, &directory, 0, 8, &supplier).await;
                application.authentication().verify(input(OPERATION, "13800000000", "fictional-token"))
                    .await.expect("retained verification result");
                application.stop().await.expect("original process drained");
                drop(application);
                let admin = connections(&server, &name, "postgres").recover().await.expect("fixture admin");
                admin.client().batch_execute("BEGIN; ALTER TABLE identity.challenges DISABLE TRIGGER USER;
                    UPDATE identity.challenges SET created_at=created_at-interval '25 hours', expires_at=expires_at-interval '25 hours', finished_at=finished_at-interval '25 hours';
                    ALTER TABLE identity.challenges ENABLE TRIGGER USER; COMMIT").await.expect("expired fixture history");
                let mut application = start(&server, &name, &directory, 0, 8, &supplier).await;
                let transport = process::transport::Transport::bind(
                    "127.0.0.1:0".parse().expect("listener"),
                    application.authentication.clone(),
                    crate::identity::authentication::Settings::new("https://example.com".into()).expect("origin"),
                    crate::source::Settings::new(vec!["127.0.0.1/32".parse().expect("proxy")]).expect("source"),
                ).await.expect("HTTP transport");
                admin.client().batch_execute("BEGIN; SELECT pg_advisory_xact_lock(1314210636, 1)")
                    .await.expect("hold retention transaction");
                application.phase = process::Phase::Running {
                    transport,
                    retention: process::retention::Retention::start(application.authentication.clone()),
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
                assert!(Instance::acquire(&directory.path().join("instance")).is_err());
                drop(application);
                assert!(Instance::acquire(&directory.path().join("instance")).is_ok());
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
                let directory = tempfile::tempdir().expect("instance directory");
                let supplier = Supplier::new().await;
                let name = database(&server).await;
                let mut application = start(&server, &name, &directory, 0, 8, &supplier).await;
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
                assert!(Instance::acquire(&directory.path().join("instance")).is_err());
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
                assert!(Instance::acquire(&directory.path().join("instance")).is_err());
                drop(application);
                assert!(Instance::acquire(&directory.path().join("instance")).is_ok());
                blocker.terminate().await.expect("fixture blocker ended");
            }).await.expect("collected failure test budget");
        });
    }

    #[test]
    fn http_stop_timeout_preserves_accepted_work_and_the_instance_lock() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let directory = tempfile::tempdir().expect("instance directory");
            let name = database(&server).await;
            let reader = connections(&server, &name, "clerk").recover().await.expect("fixture time");
            let now: chrono::DateTime<chrono::Utc> = reader.client().query_one("SELECT clock_timestamp()", &[]).await.expect("time").get(0);
            reader.terminate().await.expect("reader ends");
            let supplier = Supplier::responding(serde_json::json!({"success":true,"challenge_ts":now,"hostname":"example.com","action":"send_authentication_code","cdata":OPERATION}).to_string()).await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(8, domain::identity::message::Limits::default()).expect("settings"), process::Settings::new(Duration::from_millis(20)).expect("stop"), Instance::acquire(&directory.path().join("instance")).expect("lock"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("start");
            let (base, stop, task) = running(application).await;
            let caller = tokio::spawn(http_client().post(format!("{base}/codes")).header("operation-id", OPERATION).json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"})).send());
            let sent = timeout(Duration::from_secs(4), sms.request()).await.expect("accepted send");
            stop.send(()).expect("stop");
            let (mut application, result) = timeout(Duration::from_secs(1), task).await.expect("bounded stop").expect("HTTP task");
            let error = result.expect_err("unfinished stop");
            assert!(matches!(error, process::RunError::Application(process::Error::Timeout { .. })));
            assert!(error.incomplete());
            assert!(Instance::acquire(&directory.path().join("instance")).is_err());
            accept_sms(sent);
            assert_eq!(timeout(Duration::from_secs(3), caller).await.expect("original completion").expect("caller").expect("HTTP response").status(), 200);
            // Only the fixture waits for completion after this failure; production exits.
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
        let instance = Instance::acquire(std::path::Path::new(&value("NUCLEUS_HTTP_TEST_LOCK")))
            .expect("fixed fixture lock");
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
                Settings::new(8, domain::identity::message::Limits::default())
                    .expect("C1 settings"),
                process::Settings::new(stop).expect("stop budget"),
                instance,
                supplier,
                sms,
            );
            let settings =
                crate::identity::authentication::Settings::new("https://example.com".into())
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
                    .env("NUCLEUS_HTTP_TEST_LOCK", directory.path().join("instance"))
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
            assert!(Instance::acquire(&directory.path().join("instance")).is_err());
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
            assert!(Instance::acquire(&directory.path().join("instance")).is_err());
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
            drop(
                Instance::acquire(&directory.path().join("instance"))
                    .expect("lock reusable after confirmed exit"),
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
    fn http_recovers_the_same_message_after_confirmed_process_exit() {
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
            for attempt in 0..2 {
                let ready = TcpListener::bind("127.0.0.1:0").await.expect("readiness IPC");
                let mut command = tokio::process::Command::new(std::env::current_exe().expect("test executable"));
                command.args(["--exact","process::tests::isolated_http_process","--nocapture"])
                    .env("NUCLEUS_HTTP_TEST_DATABASE",&name)
                    .env("NUCLEUS_HTTP_TEST_PORT",server.port().to_string())
                    .env("NUCLEUS_HTTP_TEST_AUTHORITY",server.authority())
                    .env("NUCLEUS_HTTP_TEST_LOCK",directory.path().join("instance"))
                    .env("NUCLEUS_HTTP_TEST_CHALLENGE_PORT",supplier.port.to_string())
                    .env("NUCLEUS_HTTP_TEST_MESSAGE_ADDRESS",sms.address().to_string())
                    .env("NUCLEUS_HTTP_TEST_MESSAGE_AUTHORITY",&certificate)
                    .env("NUCLEUS_HTTP_TEST_READY",ready.local_addr().expect("IPC address").to_string())
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
                let mut child = command.spawn().expect("isolated HTTP child");
                let (mut socket,_) = timeout(Duration::from_secs(10),ready.accept()).await.expect("startup budget").expect("ready");
                let mut address = String::new();
                timeout(Duration::from_secs(1),socket.read_to_string(&mut address)).await.expect("readiness budget").expect("address");
                let request = client.post(format!("http://{address}/codes")).header("operation-id",OPERATION).json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"}));
                let caller = tokio::spawn(request.send());
                if attempt == 0 {
                    accept_sms(timeout(Duration::from_secs(4),sms.request()).await.expect("one real fixture call"));
                }
                let response = caller.await.expect("caller task").expect("HTTP response");
                assert_eq!(response.status(),200);
                let receipt: serde_json::Value = response.json().await.expect("message response");
                if let Some(original) = &option { assert_eq!(&receipt,original); } else { option = Some(receipt); }
                let process_id = child.id().expect("owned child PID");
                assert!(tokio::process::Command::new("kill").args(["-TERM",&process_id.to_string()]).status().await.expect("signal owned fixture").success());
                assert!(timeout(Duration::from_secs(7),child.wait()).await.expect("confirmed process exit").expect("reap owned process").success());
                drop(Instance::acquire(&directory.path().join("instance")).expect("old process released lock"));
            }
            assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst),1);
            assert!(timeout(Duration::from_millis(50),sms.request()).await.is_err(),"a fresh process cannot resend");
        });
    }

    #[test]
    fn failed_clock_connection_releases_startup_resources_before_retry() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let directory = tempfile::tempdir().expect("instance directory");
            let error = Application::start(
                connections(&server, &name, "missing_runtime_identity"),
                keys(0),
                Settings::new(2, domain::identity::message::Limits::default()).expect("settings"),
                process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                Instance::acquire(&directory.path().join("instance")).expect("instance"),
                supplier.client(),
                message().await,
            )
            .await
            .err()
            .expect("clock connection must succeed before construction");
            assert!(matches!(
                error,
                process::Error::Runtime(domain::runtime::Error::Connection(_))
            ));
            let mut application = start(&server, &name, &directory, 0, 2, &supplier).await;
            application.stop().await.expect("stop successful retry");
        });
    }

    #[test]
    fn retains_original_acceptance_and_checks_proof_before_input() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let directory = tempfile::tempdir().expect("instance directory");
            let mut service = start(&server, &name, &directory, 0, 4, &supplier).await;
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
            let mut service = start(&server, &name, &directory, 0, 4, &supplier).await;
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
            let directory = tempfile::tempdir().expect("instance directory");
            let mut service = start(&server, &name, &directory, 0, 4, &supplier).await;
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
            let directory = tempfile::tempdir().expect("instance directory");
            let mut service = Application::start(
                connections(&server, &name, "clerk"),
                keys(0),
                Settings::new(1, domain::identity::message::Limits::default()).expect("settings"),
                process::Settings::new(Duration::from_secs(1)).expect("process settings"),
                Instance::acquire(&directory.path().join("instance")).expect("instance"),
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
            assert!(Instance::acquire(&directory.path().join("instance")).is_err());
            assert!(matches!(
                service.stop().await,
                Err(process::Error::Timeout { .. })
            ));
            assert!(Instance::acquire(&directory.path().join("instance")).is_err());
            blocker
                .client()
                .batch_execute("COMMIT")
                .await
                .expect("release writes");
            service
                .stop()
                .await
                .expect("drain retained handle after cancelled and timed-out stops");
            assert!(Instance::acquire(&directory.path().join("instance")).is_err());
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
            let mut service = start(&server, &name, &directory, 0, 1, &supplier).await;
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
                    let events: Vec<serde_json::Value> = diagnostic
                        .lines()
                        .map(|line| {
                            serde_json::from_str::<serde_json::Value>(line).expect("JSON event")
                        })
                        .filter(|event| event["fields"]["stage"] == "authentication_completion")
                        .collect();
                    assert_eq!(events.len(), 1);
                    assert_eq!(events[0]["fields"]["operation_id"], OPERATION);
                    let request_id: uuid::Uuid = events[0]["spans"][0]["request_id"]
                        .as_str()
                        .expect("request ID")
                        .parse()
                        .expect("UUID");
                    assert_eq!(request_id.get_version(), Some(uuid::Version::Random));
                    if mode == "http" {
                        assert_eq!(events[0]["spans"][0]["route"], "/codes");
                    }
                    assert!(diagnostic.contains("ClockInvalid"));
                    for secret in [
                        "13800000000",
                        "fictional-private-token",
                        "fictional-secret",
                        "SELECT",
                        "UPDATE",
                    ] {
                        assert!(!diagnostic.contains(secret));
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
            let directory = tempfile::tempdir().expect("instance directory");
            let mut application = start(&server, &name, &directory, 0, 8, &supplier).await;
            let blocker = connections(&server, &name, "postgres").recover().await.expect("fixture blocker");
            blocker.client().batch_execute("BEGIN; SELECT pg_advisory_xact_lock(1314210636, 1)").await.expect("block new work");
            if std::env::var("NUCLEUS_TEST_WORKER_DIAGNOSTICS").expect("child mode") == "http" {
                let (base, stop, task) = running(application).await;
                let waiter = tokio::spawn(http_client().post(format!("{base}/codes"))
                    .header("operation-id", OPERATION)
                    .json(&serde_json::json!({"mobile":"13800000000","token":"fictional-private-token"})).send());
                wait_for_lock(blocker.client()).await;
                waiter.abort();
                assert!(waiter.await.expect_err("HTTP caller ended").is_cancelled());
                blocker.client().batch_execute("UPDATE security.clocks SET observed_at = clock_timestamp() + interval '1 hour'; COMMIT").await.expect("release into failed trusted time");
                stop.send(()).expect("stop HTTP");
                let (_, result) = timeout(Duration::from_secs(5), task).await.expect("drain budget").expect("supervisor ended");
                result.expect("retained work finishes");
                assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
                blocker.terminate().await.expect("fixture blocker ends");
                return;
            }
            let service = application.authentication().clone();
            let waiter = tokio::spawn(async move {
                service.verify(input(OPERATION, "13800000000", "fictional-private-token")).await
            }.instrument(tracing::info_span!("request", request_id = %uuid::Uuid::new_v4())));
            wait_for_lock(blocker.client()).await;
            waiter.abort();
            assert!(waiter.await.expect_err("waiter ended").is_cancelled());
            blocker.client().batch_execute("UPDATE security.clocks SET observed_at = clock_timestamp() + interval '1 hour'; COMMIT").await.expect("release into failed trusted time");
            application.stop().await.expect("wait for the retained worker");
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
            let directory = tempfile::tempdir().expect("instance directory");
            let mut service = start(&server, &name, &directory, 0, 2, &supplier).await;
            let admin = connections(&server, &name, "steward").recover().await.expect("deployment");
            admin.client().batch_execute("UPDATE security.clocks SET observed_at = clock_timestamp() + interval '1 hour'").await.expect("future high-water mark");
            let error = service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect_err("time rollback closes admission");
            assert!(matches!(error, Error::NotCommitted { cause } if matches!(*cause, Error::Runtime(domain::runtime::Error::ClockInvalid))));
            assert!(matches!(service.authentication().verify(input(OTHER, "13800000000", "other-token")).await, Err(Error::Closed)));
            let total: i64 = admin.client().query_one("SELECT count(*) FROM identity.challenges", &[]).await.expect("count").get(0);
            assert_eq!(total, 0);
            service.stop().await.expect("stop");
            drop(service);
            let error = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(2, domain::identity::message::Limits::default()).expect("settings"), process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                Instance::acquire(&directory.path().join("instance")).expect("instance"), supplier.client(), message().await).await.err().expect("restart detects rollback");
            assert!(matches!(error, process::Error::Authentication(Error::Runtime(domain::runtime::Error::ClockInvalid))));
            assert!(error.to_string().contains("observed_at not later than the new sample"));
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
        directory: &tempfile::TempDir,
        proxy: &fixture::database_proxy::Proxy,
    ) -> Application {
        Application::start(
            proxy_connections(name, proxy),
            keys(0),
            Settings::new(4, domain::identity::message::Limits::default()).expect("settings"),
            process::Settings::new(Duration::from_secs(30)).expect("process settings"),
            Instance::acquire(&directory.path().join("instance")).expect("instance"),
            supplier.client(),
            message().await,
        )
        .await
        .expect("start through proxy")
    }

    #[test]
    fn retained_history_can_take_longer_than_one_batch_budget_to_verify() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let directory = tempfile::tempdir().expect("instance directory");
            let mut application = start(&server, &name, &directory, 0, 2, &supplier).await;
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
                tokio::join!(proxy_service(&supplier, &name, &directory, &proxy), delays)
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
            let directory = tempfile::tempdir().expect("instance directory");
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            proxy.arm(fixture::database_proxy::Fault::StalledHistory);
            let startup = Application::start(
                proxy_connections(&name, &proxy),
                keys(0),
                Settings::new(2, domain::identity::message::Limits::default()).expect("settings"),
                process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                Instance::acquire(&directory.path().join("instance")).expect("instance"),
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
                Err(process::Error::Authentication(Error::Timeout(_)))
            ));
            proxy.stop().await;
            let mut application = start(&server, &name, &directory, 0, 2, &supplier).await;
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
            let directory = tempfile::tempdir().expect("instance directory");
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut service = proxy_service(&supplier, &name, &directory, &proxy).await;
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
            let directory = tempfile::tempdir().expect("instance directory");
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut service = proxy_service(&supplier, &name, &directory, &proxy).await;
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
    fn confirmed_rollback_retains_clock_and_allows_only_a_new_explicit_attempt() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let directory = tempfile::tempdir().expect("instance directory");
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut service = proxy_service(&supplier, &name, &directory, &proxy).await;
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
            let after: chrono::DateTime<chrono::Utc> = read
                .client()
                .query_one("SELECT observed_at FROM security.clocks", &[])
                .await
                .expect("clock")
                .get(0);
            assert!(after > before);
            service
                .authentication()
                .verify(input(OPERATION, "13800000000", "fictional-token"))
                .await
                .expect("explicit retry after known rollback");
            read.terminate().await.expect("end read");
            service.stop().await.expect("stop");
            proxy.stop().await;
        });
    }

    #[test]
    fn stalled_time_sampling_has_one_budget_and_closes_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let directory = tempfile::tempdir().expect("instance directory");
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut service = proxy_service(&supplier, &name, &directory, &proxy).await;
            proxy.arm(fixture::database_proxy::Fault::StalledTime);
            let worker = service.authentication().clone();
            let task = tokio::spawn(async move {
                worker
                    .verify(input(OPERATION, "13800000000", "fictional-token"))
                    .await
            });
            timeout(Duration::from_secs(5), proxy.intercepted())
                .await
                .expect("time sample held");
            assert!(matches!(
                timeout(Duration::from_secs(5), task)
                    .await
                    .expect("sample timeout")
                    .expect("worker"),
                Err(Error::NotCommitted { .. })
            ));
            proxy.release();
            assert!(matches!(
                service
                    .authentication()
                    .verify(input(OTHER, "13800000000", "other-token"))
                    .await,
                Err(Error::Closed)
            ));
            service.stop().await.expect("stop");
            proxy.stop().await;
        });
    }

    #[test]
    fn retained_token_versions_and_original_deadlines_survive_key_selection_changes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let directory = tempfile::tempdir().expect("instance directory");
            let mut service = start(&server, &name, &directory, 0, 2, &supplier).await;
            let original = service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("original");
            service.stop().await.expect("stop");
            drop(service);
            let owner = connections(&server, &name, "steward").recover().await.expect("owner");
            owner.client().batch_execute("BEGIN; UPDATE security.challenges SET active = false WHERE active; UPDATE security.challenges SET active = true WHERE id = 1; COMMIT").await.expect("offline key fixture");
            let mut service = start(&server, &name, &directory, 1, 2, &supplier).await;
            assert_eq!(service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("original key"), original);
            assert!(matches!(service.authentication().verify(input(OTHER, "13800000000", "fictional-token")).await, Err(Error::Occupied)));
            service.stop().await.expect("stop");
            drop(service);
            // Expired stored facts remain valid to decode and never acquire a fresh deadline.
            owner.client().batch_execute("ALTER TABLE identity.challenges DISABLE TRIGGER challenges_transition_check; UPDATE identity.challenges SET created_at = created_at - interval '1 hour', expires_at = expires_at - interval '1 hour'; ALTER TABLE identity.challenges ENABLE TRIGGER challenges_transition_check").await.expect("expired history fixture");
            let mut service = start(&server, &name, &directory, 1, 2, &supplier).await;
            let restored = service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("expired original receipt");
            assert_eq!(restored.id(), original.id());
            assert_eq!(restored.created_at(), original.created_at() - chrono::TimeDelta::hours(1));
            assert_eq!(restored.expires_at(), original.expires_at() - chrono::TimeDelta::hours(1));
            service.stop().await.expect("stop");
            owner.terminate().await.expect("end owner");
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
                let directory = tempfile::tempdir().expect("instance directory");
                let mut service = start(&server, &name, &directory, 0, 2, &supplier).await;
                service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("original");
                service.stop().await.expect("stop");
                drop(service);
                let owner = connections(&server, &name, "steward").recover().await.expect("owner");
                owner.client().batch_execute("ALTER TABLE identity.challenges DISABLE TRIGGER challenges_transition_check").await.expect("owner creates corrupt fixture");
                owner.client().batch_execute(corruption).await.expect("invalid retained-history fixture");
                owner.client().batch_execute("ALTER TABLE identity.challenges ENABLE TRIGGER challenges_transition_check").await.expect("restore guard");
                owner.terminate().await.expect("end owner");
                let error = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(2, domain::identity::message::Limits::default()).expect("settings"), process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                    Instance::acquire(&directory.path().join("instance")).expect("instance"), supplier.client(), message().await).await.err().expect("startup fails closed");
                assert!(matches!(error, process::Error::Authentication(Error::Runtime(domain::runtime::Error::ClockInvalid) | Error::Mobile(_) | Error::Facts { .. } | Error::Time { .. })));
                if index == 0 {
                    assert!(matches!(&error, process::Error::Authentication(Error::Time {
                        stage: domain::identity::authentication::Stage::ChallengeHistory, rule,
                    }) if rule.contains("created_at")));
                }
                if index == 1 {
                    assert!(matches!(error, process::Error::Authentication(Error::Runtime(domain::runtime::Error::ClockInvalid))));
                    assert!(error.to_string().contains("exactly one clock row"));
                }
                assert!(!format!("{error:?} {error}").contains("13800000000"));
                Instance::acquire(&directory.path().join("instance")).expect("failed startup finished cleanup");
            }
        });
    }

    #[test]
    fn runtime_permissions_are_minimal_and_startup_rejects_direct_and_inherited_excess() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let directory = tempfile::tempdir().expect("instance directory");
            let mut service = start(&server, &name, &directory, 0, 2, &supplier).await;
            service.authentication().verify(input(OPERATION, "13800000000", "fictional-token")).await.expect("valid runtime insert");
            service.stop().await.expect("stop");
            drop(service);
            let runtime = connections(&server, &name, "clerk").recover().await.expect("runtime");
            for sql in [
                "TRUNCATE identity.challenges", "UPDATE identity.challenges SET source = inet '192.0.2.2'",
                "DELETE FROM security.clocks", "INSERT INTO security.clocks VALUES (true, clock_timestamp())",
                "UPDATE security.clocks SET id = true", "CREATE TABLE identity.invalid (id integer)",
                "UPDATE security.challenges SET fingerprint = decode(repeat('00', 32), 'hex')",
                "UPDATE security.mobiles SET active = false",
                "DELETE FROM security.encryptions",
                "UPDATE security.messages SET active = false",
                "UPDATE identity.messages SET worker_id = gen_random_uuid()",
                "UPDATE identity.checks SET hash = decode(repeat('00', 32), 'hex')",
                "UPDATE identity.claims SET worker_id = gen_random_uuid()",
                "SELECT setval('identity.ordinals', 1)",
            ] {
                let error = runtime.client().batch_execute(sql).await.expect_err("runtime permission denied");
                assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE));
            }
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
                ("GRANT UPDATE (active) ON security.challenges TO clerk", "REVOKE UPDATE (active) ON security.challenges FROM clerk"),
                ("GRANT INSERT (id) ON security.mobiles TO clerk", "REVOKE INSERT (id) ON security.mobiles FROM clerk"),
                ("GRANT MAINTAIN ON security.encryptions TO clerk", "REVOKE MAINTAIN ON security.encryptions FROM clerk"),
                ("REVOKE UPDATE (status) ON identity.messages FROM clerk", "GRANT UPDATE (status) ON identity.messages TO clerk"),
                ("REVOKE UPDATE (finished_at) ON identity.messages FROM clerk", "GRANT UPDATE (finished_at) ON identity.messages TO clerk"),
                ("GRANT UPDATE (worker_id) ON identity.messages TO clerk", "REVOKE UPDATE (worker_id) ON identity.messages FROM clerk"),
                ("GRANT UPDATE (hash) ON identity.checks TO clerk", "REVOKE UPDATE (hash) ON identity.checks FROM clerk"),
                ("REVOKE DELETE ON identity.challenges FROM clerk", "GRANT DELETE ON identity.challenges TO clerk"),
                ("REVOKE DELETE ON identity.messages FROM clerk", "GRANT DELETE ON identity.messages TO clerk"),
                ("REVOKE DELETE ON identity.checks FROM clerk", "GRANT DELETE ON identity.checks TO clerk"),
                ("REVOKE DELETE ON identity.claims FROM clerk", "GRANT DELETE ON identity.claims TO clerk"),
                ("REVOKE USAGE ON SEQUENCE identity.ordinals FROM clerk", "GRANT USAGE ON SEQUENCE identity.ordinals TO clerk"),
                ("GRANT SELECT ON SEQUENCE identity.ordinals TO clerk", "REVOKE SELECT ON SEQUENCE identity.ordinals FROM clerk"),
                ("GRANT UPDATE ON SEQUENCE identity.ordinals TO clerk", "REVOKE UPDATE ON SEQUENCE identity.ordinals FROM clerk"),
                ("GRANT UPDATE (active) ON security.messages TO clerk", "REVOKE UPDATE (active) ON security.messages FROM clerk"),
                ("REVOKE INSERT (id) ON identity.claims FROM clerk", "GRANT INSERT (id) ON identity.claims TO clerk"),
                ("GRANT INSERT ON identity.challenges TO clerk", "REVOKE INSERT ON identity.challenges FROM clerk; GRANT INSERT (id, operation_id, purpose, hash, key_id, mobile, source, calls, created_at, expires_at) ON identity.challenges TO clerk"),
                ("GRANT REFERENCES (mobile) ON identity.challenges TO clerk", "REVOKE REFERENCES (mobile) ON identity.challenges FROM clerk"),
                ("GRANT CREATE ON SCHEMA security TO clerk", "REVOKE CREATE ON SCHEMA security FROM clerk"),
            ] {
                owner.client().batch_execute(grant).await.expect("excess grant");
                let error = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(2, domain::identity::message::Limits::default()).expect("settings"), process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                    Instance::acquire(&directory.path().join("instance")).expect("instance"), supplier.client(), message().await).await.err().expect("excess privilege blocks start");
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
                let error = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(2, domain::identity::message::Limits::default()).expect("settings"), process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                    Instance::acquire(&directory.path().join("instance")).expect("instance"), supplier.client(), message().await).await.err().expect("every schema is checked");
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
                let error = Application::start(connections(&server, &name, "clerk"), keys(0), Settings::new(2, domain::identity::message::Limits::default()).expect("settings"), process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                    Instance::acquire(&directory.path().join("instance")).expect("instance"), supplier.client(), message().await).await.err().expect("SET ROLE excess blocks start");
                assert!(matches!(error, process::Error::Database(domain::database::Error::Permissions)));
                admin.client().batch_execute(&revoke).await.expect("remove role permission");
            }
            admin.client().batch_execute(&format!("REVOKE {role} FROM clerk")).await.expect("remove membership");
            admin.terminate().await.expect("end admin");
            owner.terminate().await.expect("end owner");
            let mut service = start(&server, &name, &directory, 0, 2, &supplier).await;
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
        let lock = std::env::var("NUCLEUS_ACCEPTANCE_TEST_LOCK").expect("fixture lock path");
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
                Settings::new(2, domain::identity::message::Limits::default()).expect("settings"),
                process::Settings::new(Duration::from_secs(30)).expect("process settings"),
                Instance::acquire(std::path::Path::new(&lock)).expect("exclusive instance"),
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
        let directory = tempfile::tempdir().expect("instance directory");
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
                    .env(
                        "NUCLEUS_ACCEPTANCE_TEST_LOCK",
                        directory.path().join("instance"),
                    )
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
