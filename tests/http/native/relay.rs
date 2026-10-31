//! A bounded HTTP/1 ingress with exact, one-shot response loss after durable success.

use std::{
    collections::{HashMap, HashSet},
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, Response, StatusCode},
};
use http_body_util::{BodyExt as _, Limited};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::{Notify, watch},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use uuid::Uuid;

use super::facts::Facts;

pub(super) const SOURCE: &str = "203.0.113.77";
const BODY_LIMIT: usize = 1024 * 1024;
const REQUEST_LIMIT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Case {
    Registration,
    LogoutCurrent,
    LogoutAll,
    LogoutOther,
    #[serde(rename = "current_503")]
    Current503,
    #[serde(rename = "sessions_503")]
    Sessions503,
}
impl Case {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Registration => "registration",
            Self::LogoutCurrent => "logout_current",
            Self::LogoutAll => "logout_all",
            Self::LogoutOther => "logout_other",
            Self::Current503 => "current_503",
            Self::Sessions503 => "sessions_503",
        }
    }
    fn scope(self) -> Option<&'static str> {
        match self {
            Self::LogoutCurrent => Some("current"),
            Self::LogoutAll => Some("all"),
            Self::LogoutOther => Some("other"),
            _ => None,
        }
    }
    fn read(self) -> bool {
        matches!(self, Self::Current503 | Self::Sessions503)
    }
    fn path(self, target: Option<Uuid>) -> Result<String, String> {
        Ok(match self {
            Self::Registration => "/api/v1/registrations".into(),
            Self::LogoutCurrent => "/api/v1/sessions/current".into(),
            Self::LogoutAll | Self::Sessions503 => "/api/v1/sessions".into(),
            Self::LogoutOther => format!(
                "/api/v1/sessions/{}",
                target.ok_or("other requires a target")?
            ),
            Self::Current503 => "/api/v1/accounts/current".into(),
        })
    }
    fn method(self) -> Method {
        if self.read() {
            Method::GET
        } else if self == Self::Registration {
            Method::POST
        } else {
            Method::DELETE
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Capture {
    pub(super) operation: Uuid,
    pub(super) status: u16,
    pub(super) durable: bool,
    pub(super) dropped: bool,
    pub(super) actor: Option<Uuid>,
    pub(super) target: Option<Uuid>,
}
impl Capture {
    pub(super) fn evidence(&self) -> Value {
        json!({"operation": self.operation, "status": self.status, "durable": self.durable, "dropped": self.dropped, "actorSession": self.actor, "targetSession": self.target})
    }
}

struct Fault {
    case: Case,
    operation: Option<Uuid>,
    actor: Option<Uuid>,
    target: Option<Uuid>,
    registration: Option<Uuid>,
    captured: Option<Uuid>,
    result: Option<Result<Capture, String>>,
}
#[derive(Default)]
struct State {
    fault: Mutex<Option<Fault>>,
    used: Mutex<HashSet<Case>>,
    sessions: Mutex<HashMap<String, Uuid>>,
    stripped: AtomicUsize,
    notify: Notify,
}
pub(super) struct Relay {
    state: Arc<State>,
    stop: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<Result<(), String>>>>,
}
impl Relay {
    pub(super) fn start(
        listener: TcpListener,
        backend: String,
        facts: Arc<Facts>,
    ) -> Result<Arc<Self>, String> {
        let state = Arc::new(State::default());
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(25))
            .build()
            .map_err(|_| "relay client construction failed")?;
        let (stop, mut stopped) = watch::channel(false);
        let captured = Arc::clone(&state);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            let mut errors = Vec::new();
            loop {
                tokio::select! {
                    biased;
                    _ = stopped.changed() => break,
                    joined = connections.join_next(), if !connections.is_empty() => {
                        match joined { Some(Ok(Ok(()))) => {}, _ => errors.push("relay connection failed") }
                    }
                    accepted = listener.accept(), if connections.len() < 16 => {
                        let (stream, _) = accepted.map_err(|_| "relay accept failed")?;
                        let state = Arc::clone(&captured); let facts = Arc::clone(&facts); let backend = backend.clone(); let client = client.clone();
                        connections.spawn(async move {
                            let service = service_fn(move |request| forward(request, backend.clone(), Arc::clone(&state), Arc::clone(&facts), client.clone()));
                            let mut builder = http1::Builder::new();
                            builder.timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(10)).max_buf_size(16 * 1024).keep_alive(false);
                            match timeout(REQUEST_LIMIT, builder.serve_connection(TokioIo::new(stream), service)).await {
                                // User-service errors deliberately close a fully committed response.
                                Ok(Ok(())) => Ok(()),
                                Ok(Err(error)) if error.is_user() || error.is_incomplete_message() || error.is_parse() => Ok(()),
                                _ => Err("relay connection exceeded its lifetime or failed"),
                            }
                        });
                    }
                }
            }
            drop(listener);
            if timeout(Duration::from_secs(32), async {
                while let Some(result) = connections.join_next().await {
                    if !matches!(result, Ok(Ok(()))) {
                        errors.push("relay connection failed during drain");
                    }
                }
            })
            .await
            .is_err()
            {
                connections.abort_all();
                while connections.join_next().await.is_some() {}
                errors.push("relay required forced cancellation");
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(errors.join("; "))
            }
        });
        Ok(Arc::new(Self {
            state,
            stop,
            task: Mutex::new(Some(task)),
        }))
    }
    pub(super) fn arm(
        &self,
        case: Case,
        operation: Option<Uuid>,
        actor: Option<Uuid>,
        target: Option<Uuid>,
        registration: Option<Uuid>,
    ) -> Result<(), String> {
        if case == Case::Registration {
            if actor.is_some()
                || target.is_some()
                || (operation.is_none() && registration.is_none())
            {
                return Err(
                    "registration requires exact operation or known Registration id".into(),
                );
            }
        } else if actor.is_none()
            || registration.is_some()
            || (case == Case::LogoutOther) != target.is_some()
            || target.is_some_and(|id| Some(id) == actor)
        {
            return Err("fault requires exact initiator and applicable other target".into());
        }
        let mut fault = self.state.fault.lock().expect("fault lock");
        if fault.is_some() {
            return Err("another phase is active".into());
        }
        if !self.state.used.lock().expect("used cases").insert(case) {
            return Err("case already used".into());
        }
        *fault = Some(Fault {
            case,
            operation,
            actor,
            target,
            registration,
            captured: None,
            result: None,
        });
        Ok(())
    }
    pub(super) fn binding(&self, case: Case) -> Result<Value, String> {
        let fault = self.state.fault.lock().expect("fault lock");
        let fault = fault
            .as_ref()
            .filter(|fault| fault.case == case)
            .ok_or("phase binding is absent")?;
        Ok(
            json!({"method": case.method().as_str(), "path": case.path(fault.target)?, "operation": fault.operation, "actor": fault.actor, "target": fault.target, "registration": fault.registration}),
        )
    }

    pub(super) fn capture(&self, case: Case) -> Result<Option<Capture>, String> {
        let fault = self.state.fault.lock().expect("fault lock");
        let Some(fault) = fault.as_ref() else {
            return Ok(None);
        };
        if fault.case != case {
            return Err("another phase is active".into());
        }
        match fault.result.as_ref() {
            Some(Ok(capture)) => Ok(Some(capture.clone())),
            Some(Err(error)) => Err(error.clone()),
            None => Ok(None),
        }
    }
    pub(super) async fn wait_capture(
        &self,
        case: Case,
        budget: Duration,
    ) -> Result<Capture, String> {
        timeout(budget, async {
            loop {
                let notified = self.state.notify.notified();
                if let Some(value) = self.capture(case)? {
                    return Ok(value);
                }
                notified.await;
            }
        })
        .await
        .map_err(|_| "capture timed out".to_owned())?
    }
    pub(super) fn release(&self, case: Case) -> Result<(), String> {
        let mut fault = self.state.fault.lock().expect("fault lock");
        if !fault
            .as_ref()
            .is_some_and(|f| f.case == case && matches!(f.result, Some(Ok(_))))
        {
            return Err("phase has no verified capture".into());
        }
        *fault = None;
        Ok(())
    }
    pub(super) fn stripped_sources(&self) -> usize {
        self.state.stripped.load(Ordering::SeqCst)
    }
    pub(super) async fn stop(&self) -> Result<(), String> {
        let _ = self.stop.send(true);
        let task = self.task.lock().expect("relay task").take();
        match task {
            Some(task) => task
                .await
                .map_err(|_| "relay supervisor failed".to_owned())?,
            None => Err("relay stop already consumed".into()),
        }
    }
}

fn unique<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        None
    } else {
        Some(value)
    }
}
fn response(status: StatusCode, body: impl Into<Body>) -> Response<Body> {
    let mut response = Response::new(body.into());
    *response.status_mut() = status;
    response
}
fn allowed(method: &Method, path: &str) -> bool {
    match (method.as_str(), path) {
        ("POST", "/api/v1/sessions" | "/api/v1/registrations")
        | ("GET", "/api/v1/sessions" | "/api/v1/accounts/current" | "/api/v1/revocations")
        | ("DELETE", "/api/v1/sessions" | "/api/v1/sessions/current") => true,
        ("GET", value) => value.starts_with("/api/v1/avatars/"),
        ("DELETE", value) => value
            .strip_prefix("/api/v1/sessions/")
            .is_some_and(|id| id.parse::<Uuid>().is_ok()),
        _ => false,
    }
}
async fn forward(
    request: Request<Incoming>,
    backend: String,
    state: Arc<State>,
    facts: Arc<Facts>,
    client: reqwest::Client,
) -> Result<Response<Body>, io::Error> {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path();
    if parts.uri.scheme().is_some()
        || parts.uri.authority().is_some()
        || !allowed(&parts.method, path)
    {
        return Ok(response(StatusCode::NOT_FOUND, Body::empty()));
    }
    let bytes = match Limited::new(body, BODY_LIMIT).collect().await {
        Ok(value) => value.to_bytes(),
        Err(_) => return Ok(response(StatusCode::BAD_REQUEST, Body::empty())),
    };
    let operation =
        unique(&parts.headers, "operation-id").and_then(|value| value.parse::<Uuid>().ok());
    let actor = unique(&parts.headers, "authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| {
            state
                .sessions
                .lock()
                .expect("session map")
                .get(token)
                .copied()
        });
    let registration = if parts.method == Method::POST && path == "/api/v1/registrations" {
        serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|id| id.parse::<Uuid>().ok())
            })
    } else {
        None
    };
    let matched = {
        let mut fault = state.fault.lock().expect("fault lock");
        if let (Some(fault), Some(operation)) = (fault.as_mut(), operation) {
            if fault.captured.is_none()
                && parts.method == fault.case.method()
                && path == fault.case.path(fault.target).map_err(io::Error::other)?
                && fault.operation.is_none_or(|id| id == operation)
                && fault.actor.is_none_or(|id| Some(id) == actor)
                && fault.registration.is_none_or(|id| Some(id) == registration)
            {
                fault.captured = Some(operation);
                Some((fault.case, operation, fault.actor, fault.target))
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some((case, operation, actor, target)) = matched
        && case.read()
    {
        state
            .fault
            .lock()
            .expect("fault lock")
            .as_mut()
            .expect("active fault")
            .result = Some(Ok(Capture {
            operation,
            status: 503,
            durable: false,
            dropped: false,
            actor,
            target,
        }));
        state.notify.notify_waiters();
        let mut reply = response(
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"code":"dependency_unavailable"}"#,
        );
        reply
            .headers_mut()
            .insert("content-type", "application/json".parse().expect("header"));
        return Ok(reply);
    }
    let mut headers = parts.headers;
    if ["x-real-ip", "x-forwarded-for", "forwarded"]
        .iter()
        .any(|name| headers.contains_key(*name))
    {
        state.stripped.fetch_add(1, Ordering::SeqCst);
    }
    for name in [
        "host",
        "connection",
        "keep-alive",
        "proxy-connection",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "content-length",
        "expect",
        "x-real-ip",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "forwarded",
    ] {
        headers.remove(name);
    }
    headers.insert("x-real-ip", SOURCE.parse().expect("fixture source"));
    let forwarded = client
        .request(parts.method, format!("{backend}{}", parts.uri))
        .headers(headers)
        .body(bytes)
        .send()
        .await;
    let result = async {
        let forwarded = forwarded.map_err(|_| "upstream transport failed")?;
        let status = forwarded.status();
        let mut headers = forwarded.headers().clone();
        let body = forwarded
            .bytes()
            .await
            .map_err(|_| "upstream body failed")?;
        if body.len() > BODY_LIMIT {
            return Err("upstream body limit exceeded");
        }
        if status.is_success()
            && let Ok(value) = serde_json::from_slice::<Value>(&body)
            && let (Some(id), Some(token)) = (
                value["session"]["id"]
                    .as_str()
                    .and_then(|id| id.parse::<Uuid>().ok()),
                value["session"]["token"].as_str(),
            )
        {
            state
                .sessions
                .lock()
                .expect("session map")
                .insert(token.into(), id);
        }
        if let Some((case, operation, actor, target)) = matched {
            let durable = if case == Case::Registration {
                facts.durable_registration(operation).await
            } else {
                facts
                    .durable_logout(
                        operation,
                        case.scope().ok_or("scope absent")?,
                        actor.ok_or("initiator absent")?,
                    )
                    .await
            }
            .map_err(|_| "independent durability query failed")?;
            if !status.is_success() || !durable {
                return Err("matched write did not produce durable success");
            }
            state
                .fault
                .lock()
                .expect("fault lock")
                .as_mut()
                .expect("active fault")
                .result = Some(Ok(Capture {
                operation,
                status: status.as_u16(),
                durable,
                dropped: true,
                actor,
                target,
            }));
            state.notify.notify_waiters();
            return Ok(None);
        }
        for name in ["connection", "transfer-encoding", "content-length"] {
            headers.remove(name);
        }
        let mut reply = response(status, Body::from(body));
        *reply.headers_mut() = headers;
        Ok(Some(reply))
    }
    .await;
    match result {
        Ok(Some(reply)) => Ok(reply),
        Ok(None) => Err(io::Error::other(
            "fixture lost a committed success response",
        )),
        Err(reason) => {
            if matched.is_some() {
                if let Some(fault) = state.fault.lock().expect("fault lock").as_mut() {
                    fault.result = Some(Err(reason.into()));
                }
                state.notify.notify_waiters();
            }
            Ok(response(StatusCode::BAD_GATEWAY, Body::empty()))
        }
    }
}
