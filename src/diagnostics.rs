//! Online diagnostics: fixed output policy and safe request context.

use std::{io, time::Duration};

use axum::{
    extract::{MatchedPath, Request},
    http::Method,
};
use tower_http::request_id::RequestId;
use tracing::{Level, Span, field};
use tracing_subscriber::{filter::Targets, layer::SubscriberExt};

/// Install once in the executable, before loading settings or starting tasks.
pub fn init() -> Result<(), tracing::subscriber::SetGlobalDefaultError> {
    let subscriber = tracing_subscriber::registry()
        .with(
            Targets::new()
                .with_target("nucleus", Level::INFO)
                .with_target("domain::startup", Level::INFO)
                .with_target("domain::identity::authentication", Level::INFO)
                .with_target("domain::identity::revocation", Level::INFO)
                .with_target("domain::identity::transfer", Level::INFO)
                .with_target("domain::account::transfer", Level::INFO)
                .with_target("domain::registration", Level::INFO)
                .with_target("domain::suspension", Level::INFO)
                .with_target("domain::correction", Level::INFO)
                .with_target("domain::cancellation", Level::INFO)
                .with_target("domain::center::affiliation", Level::INFO)
                .with_target("domain::kernel", Level::INFO)
                .with_target("domain::access::role", Level::INFO)
                .with_target("domain::access::appointment", Level::INFO)
                .with_target("domain::access::assignment", Level::INFO)
                .with_target("domain::access::grant", Level::INFO)
                .with_target("domain::workforce::number", Level::INFO)
                .with_target("domain::runtime::work", Level::INFO)
                .with_target("domain::runtime::instance", Level::INFO),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_current_span(false)
                .with_writer(io::stderr),
        );
    tracing::subscriber::set_global_default(subscriber)?;
    // Panic payloads can contain inputs. Supervisors still report the task's final failure.
    std::panic::set_hook(Box::new(|info| {
        tracing::warn!(
            stage = "panic",
            file = info.location().map(|location| location.file()),
            line = info.location().map(|location| location.line()),
            "panic payload omitted",
        );
    }));
    Ok(())
}

pub(crate) fn request(request: &Request) -> Span {
    // Extension methods are caller-controlled text, just like unknown paths.
    let method = match *request.method() {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        Method::CONNECT => "CONNECT",
        Method::TRACE => "TRACE",
        _ => "other",
    };
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str);
    let option = request
        .extensions()
        .get::<RequestId>()
        .and_then(|id| id.header_value().to_str().ok());
    tracing::info_span!(
        "request",
        method,
        route,
        request_id = option,
        operation_id = field::Empty,
    )
}

pub(crate) fn response<B>(response: &axum::http::Response<B>, elapsed: Duration, span: &Span) {
    tracing::info!(
        parent: span,
        stage = "http_response",
        status = response.status().as_u16(),
        elapsed_ms = elapsed.as_secs_f64() * 1000.0,
        "response prepared",
    );
}

pub(crate) fn received(_: &Request, span: &Span) {
    tracing::info!(parent: span, stage = "http_request", "request received");
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) mod tests {
    use std::{collections::HashSet, process::Stdio, time::Duration};

    use axum::{http::StatusCode, routing::post};
    use serde::Deserialize;
    use serde_json::Value;
    use tokio::{net::TcpListener, time::timeout};
    use utoipa_axum::router::OpenApiRouter;
    use uuid::{Uuid, Version};

    use crate::{diagnostics, extension::Extension, json::Json, router};

    const OPERATION: &str = "550e8400-e29b-41d4-a716-446655440000";

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

    #[test]
    fn diagnostic_search_distinguishes_private_text_from_numeric_coincidences() {
        const MOBILE: &str = "13900000000";
        let diagnostic = r#"{"fields":{"elapsed_ms":0.10139000000000001,"status":401}}"#;
        assert!(
            diagnostic.contains(MOBILE),
            "reproduce the raw JSON false positive"
        );
        let event: Value = serde_json::from_str(diagnostic).expect("recorded safe event");
        assert!(!contains_text(&event, MOBILE));

        let elapsed = serde_json::json!({
            "elapsed_ms": Duration::from_nanos(2_138_000).as_secs_f64() * 1000.0,
        });
        assert!(elapsed.to_string().contains("13800000000"));
        assert!(!contains_text(&elapsed, "13800000000"));
        assert!(!contains_text(&serde_json::json!(139000000001_u64), MOBILE));

        for diagnostic in [
            r#"{"fields":{"mobile":"13900000000"}}"#,
            r#"{"fields":{"mobile":13900000000}}"#,
            r#"{"fields":{"error":"request for 13900000000 failed"}}"#,
            r#"{"spans":[{"private":{"13900000000":null}}]}"#,
            r#"{"fields":{"error":[null,false,{"input":"\u00313900000000"}]}}"#,
            r#"{"fields":{"error":[null,false,{"input":13900000000}]}}"#,
        ] {
            let event: Value = serde_json::from_str(diagnostic).expect("private event");
            assert!(
                contains_text(&event, MOBILE),
                "private text remains detectable"
            );
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        value: String,
    }

    async fn accept(Json(input): Json<Input>) -> StatusCode {
        assert_eq!(input.value, "fictional-private-body");
        StatusCode::NO_CONTENT
    }

    #[tokio::test]
    async fn requests_have_distinct_safe_context_even_when_operations_are_absent_or_repeated() {
        if std::env::var_os("NUCLEUS_TEST_REQUEST_DIAGNOSTICS").is_none() {
            let child = tokio::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", "diagnostics::tests::requests_have_distinct_safe_context_even_when_operations_are_absent_or_repeated", "--nocapture"])
                .env("NUCLEUS_TEST_REQUEST_DIAGNOSTICS", "1")
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn().expect("isolated diagnostics process");
            let output = timeout(Duration::from_secs(15), child.wait_with_output())
                .await
                .expect("child budget")
                .expect("child output");
            let text = String::from_utf8(output.stderr).expect("UTF-8 diagnostics");
            assert!(output.status.success(), "{text}");
            let events: Vec<Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).expect("JSON event"))
                .collect();
            for secret in [
                "fictional-private",
                "RUST_LOG",
                "authorization",
                "cookie",
                "13800000000",
            ] {
                assert!(
                    !events.iter().any(|event| contains_text(event, secret)),
                    "private input leaked"
                );
            }
            let responses: Vec<&Value> = events
                .iter()
                .filter(|event| event["fields"]["stage"] == "http_response")
                .collect();
            assert_eq!(responses.len(), 9);
            let received: Vec<&Value> = events
                .iter()
                .filter(|event| event["fields"]["stage"] == "http_request")
                .collect();
            assert_eq!(received.len(), responses.len());
            let mut requests = HashSet::new();
            for (started, event) in received.iter().zip(&responses) {
                let span = &event["spans"][0];
                assert_eq!(started["spans"][0]["request_id"], span["request_id"]);
                assert!(started["spans"][0].get("operation_id").is_none());
                assert_eq!(span["name"], "request");
                let id: Uuid = span["request_id"]
                    .as_str()
                    .expect("request ID")
                    .parse()
                    .expect("UUID");
                assert_eq!(id.get_version(), Some(Version::Random));
                assert_ne!(
                    id.to_string(),
                    OPERATION,
                    "external request IDs are discarded"
                );
                assert!(requests.insert(id), "each attempt has a new request ID");
                assert!(event["fields"]["elapsed_ms"].as_f64().expect("duration") >= 0.0);
            }
            for index in [0, 1, 2] {
                assert!(responses[index]["spans"][0].get("operation_id").is_none());
            }
            for event in &responses[3..] {
                assert_eq!(event["spans"][0]["operation_id"], OPERATION);
            }
            let statuses: Vec<u64> = responses
                .iter()
                .map(|event| event["fields"]["status"].as_u64().expect("status"))
                .collect();
            assert_eq!(statuses, [200, 400, 400, 404, 405, 204, 204, 422, 500]);
            assert_eq!(responses[3]["spans"][0]["route"], "unmatched");
            assert_eq!(responses[4]["spans"][0]["method"], "other");
            assert_eq!(responses[5]["spans"][0]["route"], "/api/v1/probe/{value}");
            let errors: Vec<&Value> = events
                .iter()
                .filter(|event| event["level"] == "ERROR")
                .collect();
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0]["fields"]["stage"], "request_extension");
            assert_eq!(
                errors[0]["spans"][0]["request_id"],
                responses[8]["spans"][0]["request_id"]
            );
            return;
        }
        diagnostics::init().expect("online diagnostics");
        let _ = rustls::crypto::ring::default_provider().install_default();
        // A dependency's event is disabled even if its level is ERROR.
        tracing::error!(target: "reqwest", payload = "fictional-private-provider");
        let routes = OpenApiRouter::new()
            .route("/probe/{value}", post(accept))
            .route(
                "/missing",
                post(|Extension(_): Extension<u8>| async { StatusCode::NO_CONTENT }),
            );
        let router = router::new(routes, OpenApiRouter::new()).expect("router");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .expect("HTTP server");
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .expect("local client");
        let response = client
            .get(format!("{base}/openapi/v1.json"))
            .header("operation-id", "fictional-private-id")
            .send()
            .await
            .expect("exempt request");
        assert_eq!(response.status(), 200);
        let response = client
            .post(format!("{base}/api/v1/probe/value"))
            .send()
            .await
            .expect("missing operation");
        assert_eq!(response.status(), 400);
        let response = client
            .post(format!("{base}/api/v1/probe/value"))
            .header("operation-id", "fictional-private-id")
            .send()
            .await
            .expect("invalid operation");
        assert_eq!(response.status(), 400);
        for (method, path, body, status) in [
            (
                reqwest::Method::GET,
                "/fictional-private-path?token=fictional-private-query",
                r#"{}"#,
                404,
            ),
            (
                reqwest::Method::from_bytes(b"fictional-private-method").expect("extension method"),
                "/api/v1/probe/value",
                r#"{}"#,
                405,
            ),
            (
                reqwest::Method::POST,
                "/api/v1/probe/fictional-private-path",
                r#"{"value":"fictional-private-body"}"#,
                204,
            ),
            (
                reqwest::Method::POST,
                "/api/v1/probe/fictional-private-path",
                r#"{"value":"fictional-private-body"}"#,
                204,
            ),
            (
                reqwest::Method::POST,
                "/api/v1/probe/value",
                r#"{"value":123,"13800000000":"fictional-private-body"}"#,
                422,
            ),
            (reqwest::Method::POST, "/api/v1/missing", r#"{}"#, 500),
        ] {
            let response = client
                .request(method, format!("{base}{path}"))
                .header("operation-id", OPERATION)
                .header("request-id", "fictional-private-request-id")
                .header("x-request-id", "fictional-private-request-id")
                .header("x-request-id", OPERATION)
                .header("authorization", "Bearer fictional-private-token")
                .header("cookie", "token=fictional-private-cookie")
                .header("content-type", "application/json")
                .body(body)
                .send()
                .await
                .expect("local request");
            assert_eq!(response.status(), status);
            assert!(!response.headers().contains_key("operation-id"));
            assert!(!response.headers().contains_key("request-id"));
            assert!(!response.headers().contains_key("x-request-id"));
            let _ = response.bytes().await.expect("response body");
        }
        stop.send(()).expect("stop listener");
        timeout(Duration::from_secs(3), task)
            .await
            .expect("drain budget")
            .expect("server ended");
    }

    #[tokio::test]
    async fn panic_output_omits_the_payload() {
        if std::env::var_os("NUCLEUS_TEST_PANIC_DIAGNOSTICS").is_some() {
            diagnostics::init().expect("online diagnostics");
            let result = tokio::spawn(async {
                panic!("fictional-private-panic-token");
            })
            .await;
            assert!(result.expect_err("task panicked").is_panic());
            return;
        }
        let child = tokio::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "diagnostics::tests::panic_output_omits_the_payload",
                "--nocapture",
            ])
            .env("NUCLEUS_TEST_PANIC_DIAGNOSTICS", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("isolated panic process");
        let output = timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .expect("child budget")
            .expect("child output");
        let text = String::from_utf8(output.stderr).expect("UTF-8 diagnostics");
        assert!(output.status.success(), "{text}");
        let events: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON event"))
            .collect();
        assert!(
            !events
                .iter()
                .any(|event| contains_text(event, "fictional-private"))
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["fields"]["stage"], "panic");
    }
}
