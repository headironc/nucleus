//! External native mini fixture: disposable PostgreSQL, real Application HTTP, private relay and
//! one foreground supervisor-owned lifecycle.

use std::{
    collections::{HashMap, HashSet},
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
    time::{Instant, sleep_until, timeout},
};
use uuid::Uuid;

use super::{
    control::{AbortReason, Command, Control, Observations, ReadError},
    facts::{Facts, Snapshot},
    relay::{Capture, Case, Relay},
};
use crate::{
    application::{running_at, start_mini_at},
    database::database,
    supplier::{Mini, Supplier},
};

const WINDOW: Duration = Duration::from_secs(15 * 60);
const ACCEPT: Duration = Duration::from_secs(120);
const CHECKPOINT: Duration = Duration::from_secs(8);
const STOP: Duration = Duration::from_secs(35);
const RELAY_STOP: Duration = Duration::from_secs(35);

#[derive(Clone)]
pub(super) struct Config {
    pub(super) run: Uuid,
    pub(super) run_dir: PathBuf,
    pub(super) origin: String,
    pub(super) source_revision: String,
    pub(super) openapi_sha256: String,
    pub(super) client_revision: String,
    pub(super) client_build: String,
    window: Duration,
    interrupted_cleanup: bool,
}

impl Config {
    pub(super) fn from_env() -> Result<Self, String> {
        let run = std::env::var("NATIVE_RUN_ID")
            .map_err(|_| "NATIVE_RUN_ID is required")?
            .parse::<Uuid>()
            .map_err(|_| "NATIVE_RUN_ID must be a UUID")?;
        let run_dir = PathBuf::from(
            std::env::var("NATIVE_RUN_DIR").map_err(|_| "NATIVE_RUN_DIR is required")?,
        );
        let origin = std::env::var("NATIVE_SERVICE_ORIGIN")
            .map_err(|_| "NATIVE_SERVICE_ORIGIN is required")?;
        validate_origin(&origin)?;
        let metadata = std::fs::symlink_metadata(&run_dir)
            .map_err(|error| format!("native run directory is unavailable: {error}"))?;
        if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
            return Err("native run directory must be a private 0700 directory".into());
        }
        Ok(Self {
            run,
            run_dir,
            origin,
            source_revision: identity("NATIVE_SOURCE_REVISION")?,
            openapi_sha256: identity("NATIVE_OPENAPI_SHA256")?,
            client_revision: identity("NATIVE_CLIENT_REVISION")?,
            client_build: identity("NATIVE_CLIENT_BUILD")?,
            window: WINDOW,
            interrupted_cleanup: false,
        })
    }

    fn ready_path(&self) -> PathBuf {
        self.run_dir.join("ready.json")
    }

    fn evidence_path(&self) -> PathBuf {
        self.run_dir.join("evidence.json")
    }
}

fn validate_origin(origin: &str) -> Result<u16, String> {
    let url = url::Url::parse(origin).map_err(|_| "service origin is not a URL")?;
    let port = origin
        .strip_prefix("https://127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port > 0)
        .ok_or("service origin must name an explicit nonzero loopback port")?;
    if url.scheme() != "https"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || origin != format!("https://127.0.0.1:{port}")
    {
        return Err("service origin must be exactly https://127.0.0.1:<port>".into());
    }
    Ok(port)
}

fn identity(key: &str) -> Result<String, String> {
    let value = std::env::var(key).map_err(|_| format!("{key} is required"))?;
    if value.is_empty()
        || value.len() > 256
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'-' | b'_' | b':' | b'/' | b'+' | b'@')
        })
    {
        return Err(format!("{key} must be a bounded identity"));
    }
    Ok(value)
}

fn supplier_evidence(supplier: &Mini) -> Value {
    json!({
        "phoneExchanges": supplier.calls.load(std::sync::atomic::Ordering::SeqCst),
        "tokenExchanges": supplier.tokens.load(std::sync::atomic::Ordering::SeqCst),
        "distinctCodes": supplier.distinct_codes(),
    })
}

fn atomic_private(path: &Path, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|_| "private fixture evidence serialization failed".to_owned())?;
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| format!("private fixture file creation failed: {error}"))?;
    file.write_all(&bytes)
        .map_err(|error| format!("private fixture file write failed: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("private fixture file sync failed: {error}"))?;
    drop(file);
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("private fixture permissions failed: {error}"))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("private fixture file publish failed: {error}"))
}

struct Session {
    config: Config,
    server: Option<fixture::Supervised>,
    mini: Option<Mini>,
    supplier: Option<Supplier>,
    relay: Arc<Relay>,
    facts: Arc<Facts>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<
        JoinHandle<(
            nucleus::process::Application,
            Result<(), nucleus::process::RunError>,
        )>,
    >,
    armed: HashSet<Case>,
    checked: HashSet<Case>,
    snapshots: HashMap<Case, Snapshot>,
    captures: HashMap<Case, Capture>,
    observations: Observations,
    rejections: usize,
    failure: Option<String>,
    final_snapshot: Option<Snapshot>,
}

enum Reason {
    Finish,
    Abort(AbortReason),
    Disconnect,
    Timeout,
    Signal(&'static str),
    Failure(String),
}

impl Reason {
    fn label(&self) -> &'static str {
        match self {
            Self::Finish => "finish",
            Self::Abort(reason) => reason.label(),
            Self::Disconnect => "control_disconnect",
            Self::Timeout => "deadline",
            Self::Signal(signal) => signal,
            Self::Failure(_) => "control_failure",
        }
    }
}

pub(super) async fn execute(config: Config) -> Result<(), String> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| "SIGTERM registration failed")?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|_| "SIGINT registration failed")?;
    let port = validate_origin(&config.origin)?;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|error| format!("service origin port cannot be owned: {error}"))?;
    let control_path = config.run_dir.join("control.sock");
    let mut control = Control::bind(&control_path)?;

    let server = tokio::task::spawn_blocking(fixture::Supervised::start)
        .await
        .map_err(|_| "PG startup failed")?;
    crate::database::roles(&server).await;
    let name = database(&server).await;
    let supplier = Supplier::new().await;
    let mini = Mini::start().await;
    let application = start_mini_at(&server, &name, &supplier, &mini, &config.origin).await;
    let (backend, stop, task) = running_at(application, config.origin.clone()).await;
    let (facts, setup_failure) = match Facts::connect(&server, &name).await {
        Ok(facts) => (Arc::new(facts), None),
        Err(error) => (Arc::new(Facts::unavailable()), Some(error)),
    };
    let relay = Relay::start(listener, backend, Arc::clone(&facts))?;
    let deadline = Instant::now() + config.window;
    let ready = json!({
        "run": config.run,
        "serviceOrigin": config.origin,
        "relayBase": format!("http://127.0.0.1:{port}"),
        "control": control_path,
        "windowSeconds": config.window.as_secs(),
        "sourceRevision": config.source_revision,
        "openapiSha256": config.openapi_sha256,
        "clientRevision": config.client_revision,
        "clientBuild": config.client_build,
    });
    let mut session = Session {
        config,
        server: Some(server),
        mini: Some(mini),
        supplier: Some(supplier),
        relay,
        facts,
        stop: Some(stop),
        task: Some(task),
        armed: HashSet::new(),
        checked: HashSet::new(),
        snapshots: HashMap::new(),
        captures: HashMap::new(),
        observations: Observations::default(),
        rejections: 0,
        failure: setup_failure,
        final_snapshot: None,
    };
    let reason = if let Some(error) = session.failure.clone() {
        Reason::Failure(error)
    } else if let Err(error) = atomic_private(&session.config.ready_path(), &ready) {
        Reason::Failure(error)
    } else {
        println!("READY {}", session.config.run);
        let accepted = tokio::select! {
            _ = terminate.recv() => Err(Reason::Signal("sigterm")),
            _ = interrupt.recv() => Err(Reason::Signal("sigint")),
            result = control.accept(ACCEPT) => result.map_err(|_| Reason::Failure("private control did not connect".into())),
        };
        match accepted {
            Ok(()) => {
                drive(
                    &mut session,
                    &mut control,
                    deadline,
                    &mut terminate,
                    &mut interrupt,
                )
                .await
            }
            Err(reason) => reason,
        }
    };
    let report = session.shutdown(&reason).await;
    if matches!(reason, Reason::Finish | Reason::Abort(_)) {
        control.respond(&report).await?;
    }
    control.close().await?;
    finish(report)
}

fn finish(report: Value) -> Result<(), String> {
    if report["status"] == "passed" || report["status"] == "stopped" {
        Ok(())
    } else {
        Err("native harness did not pass its scenario and cleanup checks".into())
    }
}

async fn drive(
    session: &mut Session,
    control: &mut Control,
    deadline: Instant,
    terminate: &mut tokio::signal::unix::Signal,
    interrupt: &mut tokio::signal::unix::Signal,
) -> Reason {
    let mut hello = false;
    let deadline = deadline_future(deadline);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return Reason::Timeout,
            _ = terminate.recv() => return Reason::Signal("sigterm"),
            _ = interrupt.recv() => return Reason::Signal("sigint"),
            command = control.read() => {
                match command {
                    Ok(command) => {
                        let run = command.run();
                        if run != session.config.run {
                            if control.reject("invalid_run").await.is_err() {
                                return Reason::Disconnect;
                            }
                            continue;
                        }
                        match command {
                            Command::Hello { client_revision, client_build, .. } => {
                                if hello {
                                    if control.reject("out_of_order").await.is_err() { return Reason::Disconnect; }
                                } else if client_revision != session.config.client_revision
                                    || client_build != session.config.client_build
                                {
                                    if control.reject("identity_mismatch").await.is_err() { return Reason::Disconnect; }
                                } else {
                                    hello = true;
                                    let ready = json!({
                                        "ok": true,
                                        "code": "ready",
                                        "run": session.config.run,
                                        "serviceOrigin": session.config.origin,
                                        "windowSeconds": session.config.window.as_secs(),
                                        "sourceRevision": session.config.source_revision,
                                        "openapiSha256": session.config.openapi_sha256,
                                    });
                                    if control.respond(&ready).await.is_err() { return Reason::Disconnect; }
                                }
                            }
                            Command::Arm { case, operation, actor, target, registration, .. } => {
                                if !hello {
                                    if control.reject("out_of_order").await.is_err() { return Reason::Disconnect; }
                                    continue;
                                }
                                match arm(session, case, operation, actor, target, registration).await {
                                    Ok(reply) => { if control.respond(&reply).await.is_err() { return Reason::Disconnect; } }
                                    Err((code, _message)) => {
                                        session.rejections += 1;
                                        if control.reject(code).await.is_err() { return Reason::Disconnect; }
                                    }
                                }
                            }
                            Command::Snapshot { .. } => {
                                if !hello {
                                    if control.reject("out_of_order").await.is_err() { return Reason::Disconnect; }
                                    continue;
                                }
                                match snapshot_reply(session).await {
                                    Ok(reply) => { if control.respond(&reply).await.is_err() { return Reason::Disconnect; } }
                                    Err(message) => {
                                        session.note_failure(message);
                                        if control.reject("fact_failure").await.is_err() { return Reason::Disconnect; }
                                    }
                                }
                            }
                            Command::Checkpoint { case, operation, actor, target, later_session, foreign_session, .. } => {
                                if !hello {
                                    if control.reject("out_of_order").await.is_err() { return Reason::Disconnect; }
                                    continue;
                                }
                                match checkpoint(session, case, operation, actor, target, later_session, foreign_session).await {
                                    Ok(reply) => {
                                        session.checked.insert(case);
                                        if control.respond(&reply).await.is_err() { return Reason::Disconnect; }
                                    }
                                    Err((code, message)) => {
                                        session.note_failure(message);
                                        if control.reject(code).await.is_err() { return Reason::Disconnect; }
                                    }
                                }
                            }
                            Command::Finish { observations, .. } => {
                                if !hello {
                                    if control.reject("out_of_order").await.is_err() { return Reason::Disconnect; }
                                    continue;
                                }
                                session.observations = observations;
                                return Reason::Finish;
                            }
                            Command::Abort { reason, .. } => {
                                if !hello {
                                    if control.reject("out_of_order").await.is_err() { return Reason::Disconnect; }
                                    continue;
                                }
                                return Reason::Abort(reason);
                            }
                        }
                    }
                    Err(ReadError::Disconnected) => return Reason::Disconnect,
                    Err(ReadError::Timeout) => return Reason::Failure("control command wait expired".into()),
                    Err(ReadError::Malformed) => {
                        session.rejections += 1;
                        if control.reject("invalid_control").await.is_err() { return Reason::Disconnect; }
                    }
                    Err(ReadError::Io(message)) => return Reason::Failure(message),
                }
            }
        }
    }
}

async fn deadline_future(deadline: Instant) {
    sleep_until(deadline).await;
}

impl Session {
    fn note_failure(&mut self, message: String) {
        if self.failure.is_none() {
            self.failure = Some(message);
        }
    }

    async fn shutdown(self, reason: &Reason) -> Value {
        let config = self.config.clone();
        match timeout(Duration::from_secs(150), self.drain(reason)).await {
            Ok(report) => report,
            Err(_) => {
                let report = json!({"run": config.run, "status": "failed", "scenario": {"outcome": reason.label(), "passed": false}, "cleanup": {"errors": ["global drain deadline expired; retained resources require supervisor readback"]}, "leaks": true});
                let _ = atomic_private(&config.evidence_path(), &report);
                report
            }
        }
    }

    async fn drain(mut self, reason: &Reason) -> Value {
        if let Reason::Failure(message) = reason
            && self.failure.is_none()
        {
            self.failure = Some(message.clone());
        }
        if let Reason::Abort(abort) = reason
            && self.failure.is_none()
        {
            self.failure = Some(format!("client aborted: {}", abort.label()));
        }
        let scenario_present = self.armed.iter().all(|case| self.checked.contains(case));
        let complete = [
            Case::Registration,
            Case::LogoutCurrent,
            Case::LogoutAll,
            Case::LogoutOther,
            Case::Current503,
            Case::Sessions503,
        ]
        .iter()
        .all(|case| self.checked.contains(case));
        let observations_ok = self
            .observations
            .evidence()
            .as_object()
            .is_some_and(|fields| fields.values().all(|value| value == &json!(true)));
        let scenario_ok = matches!(reason, Reason::Finish)
            && complete
            && observations_ok
            && scenario_present
            && self.failure.is_none();
        if self.failure.is_none() && !scenario_present {
            self.failure = Some("an armed case was not checked before finish".into());
        }
        let mut errors = Vec::new();
        let relay = match timeout(RELAY_STOP, self.relay.stop()).await {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                errors.push(error);
                false
            }
            Err(_) => {
                errors.push("private relay did not drain before its budget".into());
                false
            }
        };
        let stopped = self
            .stop
            .take()
            .map(|stop| stop.send(()).is_ok())
            .unwrap_or(false);
        let application = match self.task.take() {
            Some(mut task) => match timeout(STOP, &mut task).await {
                Ok(Ok((mut application, result))) => {
                    if let Err(error) = result {
                        errors.push(format!("application serve failed: {error}"));
                    }
                    match application.stop().await {
                        Ok(()) => true,
                        Err(error) => {
                            errors.push(format!("application drain failed: {error}"));
                            false
                        }
                    }
                }
                Ok(Err(error)) => {
                    errors.push(format!("application task ended unexpectedly: {error}"));
                    false
                }
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    errors.push("application drain exceeded its budget".into());
                    false
                }
            },
            None => false,
        };
        let final_snapshot = match self.facts.snapshot().await {
            Ok(snapshot) => {
                self.final_snapshot = Some(snapshot);
                true
            }
            Err(error) => {
                errors.push(error);
                false
            }
        };
        let observer = match self.facts.stop().await {
            Ok(()) => true,
            Err(error) => {
                errors.push(error);
                false
            }
        };
        let supplier_counts = self.mini.as_ref().map(supplier_evidence);
        let mini = match self.mini.take() {
            Some(mini) => match mini.stop().await {
                Ok(()) => true,
                Err(error) => {
                    errors.push(error);
                    false
                }
            },
            None => false,
        };
        let supplier = match self.supplier.take() {
            Some(supplier) => match supplier.stop().await {
                Ok(()) => true,
                Err(error) => {
                    errors.push(error);
                    false
                }
            },
            None => false,
        };
        let postgres = match self.server.take() {
            Some(server)
                if self.config.interrupted_cleanup || !application || !relay || !observer =>
            {
                let retained = server.retain();
                errors.push(format!(
                    "PostgreSQL termination is unconfirmed; retained {}",
                    retained.display()
                ));
                false
            }
            Some(server) => match tokio::task::spawn_blocking(move || server.stop()).await {
                Ok(Ok(())) => true,
                result => {
                    let error = match result {
                        Ok(Err(error)) => error,
                        _ => "PostgreSQL cleanup task failed".into(),
                    };
                    errors.push(error);
                    false
                }
            },
            None => false,
        };
        let cleanup_ok = relay
            && stopped
            && application
            && final_snapshot
            && observer
            && mini
            && supplier
            && postgres
            && errors.is_empty();
        let status = if scenario_ok && cleanup_ok {
            "passed"
        } else if matches!(reason, Reason::Finish)
            && self.armed.is_empty()
            && cleanup_ok
            && self.failure.is_none()
        {
            "stopped"
        } else {
            "failed"
        };
        let captures: Vec<Value> = self
            .captures
            .iter()
            .map(|(case, capture)| {
                let mut value = capture.evidence();
                value["case"] = json!(case.label());
                value
            })
            .collect();
        let report = json!({
            "run": self.config.run,
            "status": status,
            "scenario": {
                "outcome": reason.label(),
                "passed": scenario_ok,
                "reason": self.failure,
                "rejections": self.rejections,
            },
            "source": {
                "sourceRevision": self.config.source_revision,
                "openapiSha256": self.config.openapi_sha256,
                "clientRevision": self.config.client_revision,
                "clientBuild": self.config.client_build,
            },
            "serviceOrigin": self.config.origin,
            "captures": captures,
            "supplier": supplier_counts,
            "observations": self.observations.evidence(),
            "cleanup": {
                "privateRelay": relay,
                "applicationStopSignal": stopped,
                "applicationDrain": application,
                "independentFacts": final_snapshot,
                "observer": observer,
                "miniSupplier": mini,
                "unusedCaptchaSupplier": supplier,
                "postgresql": postgres,
                "errors": errors,
            },
            "finalSnapshot": self.final_snapshot.as_ref().map(Snapshot::evidence),
            "leaks": !cleanup_ok,
        });
        if let Err(error) = atomic_private(&self.config.evidence_path(), &report) {
            return json!({
                "run": self.config.run,
                "status": "failed",
                "scenario": {"outcome": reason.label(), "passed": false, "reason": error},
                "cleanup": {"errors": [error]},
                "leaks": true,
            });
        }
        report
    }
}

async fn arm(
    session: &mut Session,
    case: Case,
    operation: Option<Uuid>,
    actor: Option<Uuid>,
    target: Option<Uuid>,
    registration: Option<Uuid>,
) -> Result<Value, (&'static str, String)> {
    session
        .relay
        .arm(case, operation, actor, target, registration)
        .map_err(|message| ("invalid_fault", message))?;
    let snapshot = session
        .facts
        .snapshot()
        .await
        .map_err(|message| ("fact_failure", message))?;
    session.armed.insert(case);
    session.snapshots.insert(case, snapshot);
    let supplier = session
        .mini
        .as_ref()
        .map(supplier_evidence)
        .unwrap_or_else(|| json!({}));
    Ok(json!({
        "ok": true,
        "code": "armed",
        "case": case.label(),
        "operation": operation,
        "actorSession": actor,
        "targetSession": target,
        "supplier": supplier,
    }))
}

async fn snapshot_reply(session: &Session) -> Result<Value, String> {
    let snapshot = session.facts.snapshot().await?;
    let supplier = session
        .mini
        .as_ref()
        .map(supplier_evidence)
        .unwrap_or_else(|| json!({}));
    Ok(json!({
        "ok": true,
        "code": "snapshot",
        "counts": snapshot.evidence(),
        "supplier": supplier,
        "relay": {
            "fixedSource": super::relay::SOURCE,
            "strippedSourceRequests": session.relay.stripped_sources(),
        },
    }))
}

async fn checkpoint(
    session: &mut Session,
    case: Case,
    operation: Option<Uuid>,
    actor: Option<Uuid>,
    target: Option<Uuid>,
    later_session: Option<Uuid>,
    foreign_session: Option<Uuid>,
) -> Result<Value, (&'static str, String)> {
    if !session.armed.contains(&case) {
        return Err(("out_of_order", "checkpoint before arm".into()));
    }
    let capture = session
        .relay
        .wait_capture(case, CHECKPOINT)
        .await
        .map_err(|message| ("fault_not_captured", message))?;
    if let Some(operation) = operation
        && operation != capture.operation
    {
        return Err((
            "operation_mismatch",
            "checkpoint operation does not own the capture".into(),
        ));
    }
    if capture.actor != actor {
        return Err((
            "actor_mismatch",
            "checkpoint actor does not own the capture".into(),
        ));
    }
    if capture.target != target {
        return Err((
            "target_mismatch",
            "checkpoint target does not own the capture".into(),
        ));
    }
    let facts = match case {
        Case::Registration => session.facts.registration(capture.operation).await,
        Case::LogoutCurrent | Case::LogoutAll | Case::LogoutOther => {
            if matches!(case, Case::LogoutCurrent | Case::LogoutAll) && later_session.is_none() {
                return Err((
                    "invalid_checkpoint",
                    "later same-account login is required".into(),
                ));
            }
            if case == Case::LogoutOther && foreign_session.is_none() {
                return Err((
                    "invalid_checkpoint",
                    "foreign-account session is required".into(),
                ));
            }
            let actor = actor.ok_or((
                "invalid_checkpoint",
                "logout checkpoint needs its initiator".to_owned(),
            ))?;
            session
                .facts
                .logout(
                    capture.operation,
                    actor,
                    target,
                    later_session,
                    foreign_session,
                )
                .await
        }
        Case::Current503 | Case::Sessions503 => {
            let actor = actor.ok_or((
                "invalid_checkpoint",
                "dependency checkpoint needs its session".to_owned(),
            ))?;
            let armed = session.snapshots.get(&case).ok_or((
                "out_of_order",
                "dependency checkpoint has no armed snapshot".to_owned(),
            ))?;
            let mut facts = session
                .facts
                .retention(actor, armed)
                .await
                .map_err(|message| ("fact_failure", message))?;
            let snapshot = session
                .facts
                .snapshot()
                .await
                .map_err(|message| ("fact_failure", message))?;
            facts["unchanged"] = json!(snapshot.state == armed.state);
            if snapshot.state != armed.state {
                return Err((
                    "write_during_dependency_failure",
                    "dependency failure changed identity facts".into(),
                ));
            }
            Ok(facts)
        }
    }
    .map_err(|message| ("fact_failure", message))?;
    let supplier = session
        .mini
        .as_ref()
        .map(supplier_evidence)
        .unwrap_or_else(|| json!({}));
    let reply = json!({
        "ok": true,
        "code": "checked",
        "case": case.label(),
        "capture": capture.evidence(),
        "facts": facts,
        "supplier": supplier,
    });
    let original = json!({
        "run": session.config.run,
        "sourceRevision": session.config.source_revision,
        "clientRevision": session.config.client_revision,
        "clientBuild": session.config.client_build,
        "binding": session.relay.binding(case).map_err(|message| ("binding_failure", message))?,
        "before": session.snapshots.get(&case).map(Snapshot::evidence),
        "laterSession": later_session,
        "foreignSession": foreign_session,
        "result": reply,
    });
    atomic_private(
        &session
            .config
            .run_dir
            .join(format!("checkpoint-{}.json", case.label())),
        &original,
    )
    .map_err(|message| ("evidence_failure", message))?;
    session.captures.insert(case, capture);
    session
        .relay
        .release(case)
        .map_err(|message| ("fault_release", message))?;
    Ok(reply)
}

#[test]
#[ignore = "external native mini harness; run tests/http/native/runner.ts"]
fn native_harness() {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => panic!("native harness configuration failed: {error}"),
    };
    let runtime = tokio::runtime::Runtime::new().expect("native harness runtime");
    match runtime.block_on(execute(config)) {
        Ok(()) => {}
        Err(error) => panic!("native harness failed: {error}"),
    }
}

#[test]
#[ignore = "private deadline self-proof entry; external native mode always uses fifteen minutes"]
fn native_deadline_harness() {
    let mut config = Config::from_env().expect("deadline self-proof config");
    config.window = Duration::from_secs(1);
    let result = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(execute(config));
    assert!(
        result.is_err(),
        "deadline cannot pass the business scenario"
    );
}

#[test]
#[ignore = "private cleanup failure self-proof entry; requires the foreground recovery supervisor"]
fn native_cleanup_harness() {
    let mut config = Config::from_env().expect("cleanup self-proof config");
    config.interrupted_cleanup = true;
    let result = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(execute(config));
    assert!(result.is_err(), "unknown cleanup cannot pass");
}

#[test]
fn origin_is_an_exact_task_loopback_endpoint() {
    for port in [1, 443, 9443, 65535] {
        assert_eq!(
            validate_origin(&format!("https://127.0.0.1:{port}")),
            Ok(port)
        );
    }
    for origin in [
        "https://127.0.0.1:0",
        "https://127.0.0.1:65536",
        "https://127.0.0.1:09443",
        "http://127.0.0.1:9443",
        "https://localhost:9443",
        "https://127.0.0.1:9443/",
        "https://127.0.0.1:9443?x=1",
        "https://127.0.0.1:9443#x",
        "https://fixture@127.0.0.1:9443",
    ] {
        assert!(validate_origin(origin).is_err());
    }
}
