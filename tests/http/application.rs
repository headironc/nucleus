//! Assembly of the application under test.
//!
//! `Application::start` takes domain values, so this module and `database` are the only modules
//! that name domain types. Capability modules use these helpers and the public HTTP surface.

use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use domain::{
    identity::{authentication::Input, avatar::link::Link},
    keys::{Entry, Id, Material},
    startup::Settings,
};
use nucleus::process::{self, Application};
use serde_json::Value;
use tokio::{task::JoinHandle, time::timeout};
use zeroize::Zeroizing;

use crate::{
    database::connections,
    supplier::{Supplier, message},
};

/// Two fictional root versions; `active` selects the one every use derives from.
pub(crate) fn keys(active: i64) -> domain::keys::Keys {
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

pub(crate) fn session_settings() -> domain::identity::authentication::session::settings::Settings {
    domain::identity::authentication::session::settings::Settings::new(
        "website-fixture".parse().expect("app"),
        86400,
        600,
        600,
    )
    .expect("session settings")
}

pub(crate) fn avatar_settings() -> Link {
    avatar_settings_at("https://avatars.example.com", Duration::from_secs(86400))
}

pub(crate) fn avatar_settings_at(origin: &str, lifetime: Duration) -> Link {
    Link::new(origin.into(), lifetime).expect("avatar settings")
}

pub(crate) fn services_with_avatar(
    capacity: usize,
    avatar: Link,
) -> Result<Settings, domain::startup::CapacityError> {
    Settings::new(
        capacity,
        domain::identity::message::Limits::default(),
        session_settings(),
        domain::registration::Limits::default(),
        avatar,
    )
    .map(|settings| settings.with_avatar_storage(Some(crate::database::avatar_storage())))
}

pub(crate) async fn start(
    server: &fixture::Server,
    name: &str,
    active: i64,
    capacity: usize,
    supplier: &Supplier,
) -> Application {
    Application::start(
        connections(server, name, "clerk"),
        keys(active),
        services(capacity).expect("settings"),
        process::Settings::new(Duration::from_secs(30)).expect("process settings"),
        supplier.client(),
        message().await,
    )
    .await
    .expect("start acceptance")
}

pub(crate) async fn start_mini(
    server: &fixture::Server,
    name: &str,
    supplier: &Supplier,
    mini: &crate::supplier::Mini,
) -> Application {
    let settings = Settings::new(
        8,
        domain::identity::message::Limits::default(),
        session_settings()
            .with_mini("mini-fixture".parse().expect("mini identity"), 2_592_000)
            .expect("mini lifetimes"),
        domain::registration::Limits::default(),
        avatar_settings(),
    )
    .expect("work capacity")
    .with_avatar_storage(Some(crate::database::avatar_storage()));
    Application::start_with_mini(
        connections(server, name, "clerk"),
        keys(0),
        settings,
        process::Settings::new(Duration::from_secs(30)).expect("stop budget"),
        supplier.client(),
        message().await,
        Some(mini.client()),
    )
    .await
    .expect("mini application")
}

/// Fixture service settings with the given work capacity and default limits.
pub(crate) fn services(capacity: usize) -> Result<Settings, domain::startup::CapacityError> {
    services_with_avatar(capacity, avatar_settings())
}

pub(crate) fn input(operation_id: &str, mobile: &str, token: &str) -> Input {
    Input::new(
        operation_id.parse().expect("operation"),
        mobile.parse().expect("mobile"),
        token.parse().expect("token"),
        "::ffff:192.0.2.1".parse().expect("source"),
    )
}

pub(crate) const OPERATION: &str = "00000000-0000-0000-0000-000000000000";

pub(crate) const OTHER: &str = "ffffffff-ffff-ffff-ffff-ffffffffffff";

pub(crate) async fn running(
    application: Application,
) -> (
    String,
    tokio::sync::oneshot::Sender<()>,
    JoinHandle<(Application, Result<(), process::RunError>)>,
) {
    running_at(application, "https://example.com".into()).await
}

pub(crate) async fn running_at(
    mut application: Application,
    origin: String,
) -> (
    String,
    tokio::sync::oneshot::Sender<()>,
    JoinHandle<(Application, Result<(), process::RunError>)>,
) {
    let (ready, address) = tokio::sync::oneshot::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let settings = nucleus::identity::settings::Settings::new(origin, "fixture-site-key".into())
        .expect("origin settings");
    let source =
        nucleus::source::Settings::new(vec!["127.0.0.0/8".parse().expect("fixture proxy")])
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

pub(crate) fn http_client() -> reqwest::Client {
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

/// Search JSON diagnostics for private text, including exact unsigned numbers.
pub(crate) fn contains_text(value: &Value, secret: &str) -> bool {
    match value {
        Value::String(text) => text.contains(secret),
        Value::Array(values) => values.iter().any(|value| contains_text(value, secret)),
        Value::Object(fields) => fields
            .iter()
            .any(|(key, value)| key.contains(secret) || contains_text(value, secret)),
        Value::Number(number) => number.is_u64() && number.to_string() == secret,
        _ => false,
    }
}

// White-box probes of domain values that have no public read. Each keeps the original assertion.

/// Registration startup requires the initialized default institutional role.
pub(crate) fn registration_role_missing(result: &Result<Application, process::Error>) -> bool {
    matches!(
        result,
        Err(process::Error::Services(
            domain::startup::Error::Registration(domain::registration::Error::Role(
                domain::access::role::Error::Facts {
                    rule: "registration retains its default institutional role",
                }
            ))
        ))
    )
}

/// Startup rejects authentic role history whose retained state is missing.
pub(crate) fn role_facts_rejected(result: &Result<Application, process::Error>) -> bool {
    matches!(
        result,
        Err(process::Error::Services(domain::startup::Error::Role(
            domain::kernel::Error::Corrupt(fault)
        ))) if fault.stage() == domain::kernel::Stage::Command
            && matches!(fault.kind(), domain::kernel::FaultKind::History(
                domain::history::Error::Integrity("every record belongs to a retained state")
            ))
    )
}

/// The persisted Message proof row matches the code sent to PNVS.
pub(crate) fn message_proof_matches(row: &tokio_postgres::Row, code: &str) -> bool {
    let id: domain::identity::message::Id = row.get("id");
    keys(0)
        .identity
        .message
        .fetch(row.get("key_id"))
        .expect("original proof key")
        .verify(
            id,
            &code.parse().expect("actual generated code"),
            &row.get("hash"),
        )
        .expect("original proof matches request code")
}

/// The text parses as a domain Message identifier.
pub(crate) fn is_message_id(text: &str) -> bool {
    text.parse::<domain::identity::message::Id>().is_ok()
}

/// The body deserializes as a group creation and serializes back for a request.
pub(crate) fn group_creation(body: &Value) -> impl serde::Serialize + use<> {
    serde_json::from_value::<domain::workforce::directory::group::Creation>(body.clone())
        .expect("the shared body is a valid group creation")
}

/// The response item deserializes as a typed position and returns its serialized ID.
pub(crate) fn typed_position_id(item: &Value) -> Value {
    let typed: domain::workforce::directory::position::Item =
        serde_json::from_value(item.clone()).expect("position response");
    serde_json::to_value(typed.id()).expect("position ID")
}

/// Run the child half of an isolated diagnostics scenario after `diagnostics::init`.
///
/// The production panic hook replaces every panic payload, so a failing child assertion would
/// reach CI only as "panic payload omitted". This keeps that hook and also records each panic's
/// source location, never its payload: assertion operands may contain protected fixture state
/// such as credential hashes. When the scenario fails, it writes the locations to stderr as one
/// `TEST_FAILURE` JSON line, which the parent reports when it asserts the child's success, before
/// checking the diagnostics. Successful children write nothing extra.
pub(crate) fn isolated_child(scenario: impl FnOnce()) {
    let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let production = std::panic::take_hook();
    let sink = std::sync::Arc::clone(&recorded);
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut panics) = sink.lock() {
            panics.push(serde_json::json!({
                "location": info.location().map(ToString::to_string),
            }));
        }
        production(info);
    }));
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(scenario)) {
        let panics = recorded
            .lock()
            .map(|panics| panics.clone())
            .unwrap_or_default();
        eprintln!(
            "{}",
            serde_json::json!({"level": "TEST_FAILURE", "fields": {"panics": panics}})
        );
        std::panic::resume_unwind(payload);
    }
}
