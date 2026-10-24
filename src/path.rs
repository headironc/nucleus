//! Axum path extraction with application rejections.

use std::fmt;

use axum::{
    extract::{FromRequestParts, rejection::PathRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

use crate::{
    json::Json,
    response::{Common, Failure},
};

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(Error))]
pub(crate) struct Path<T>(pub(crate) T);

#[derive(Error)]
#[error("request path extraction failed")]
pub(crate) struct Error(#[from] PathRejection);

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code) = match &self.0 {
            PathRejection::FailedToDeserializePathParams(error)
                if error.status() == StatusCode::BAD_REQUEST =>
            {
                tracing::info!(stage = "path_input", kind = "invalid_input", error = ?self);
                (StatusCode::BAD_REQUEST, Common::InvalidInput)
            }
            _ => {
                tracing::error!(stage = "path_input", kind = "internal", error = ?self);
                (StatusCode::INTERNAL_SERVER_ERROR, Common::InternalError)
            }
        };
        (status, Json(Failure { code })).into_response()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::io::{Read, Seek, SeekFrom};

    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::{FromRequestParts, State},
        http::{Request, StatusCode},
        response::{IntoResponse, Response},
        routing::get,
    };
    use domain::identity::operation;
    use tokio::net::TcpListener;
    use tracing::Dispatch;

    use crate::path::{Error, Path};

    async fn inspect(
        State(dispatch): State<Dispatch>,
        path: Result<Path<operation::Id>, Error>,
    ) -> Response {
        match path {
            Ok(Path(id)) => id.to_string().into_response(),
            Err(error) => {
                assert_eq!(format!("{error:?}"), "request path extraction failed");
                tracing::dispatcher::with_default(&dispatch, || error.into_response())
            }
        }
    }

    #[tokio::test]
    async fn identifiers_and_wiring_failures_keep_request_values_private() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut log = tempfile::tempfile().expect("log file");
        let dispatch = Dispatch::new(
            tracing_subscriber::fmt()
                .json()
                .without_time()
                .with_writer(log.try_clone().expect("log writer"))
                .finish(),
        );
        let (mut parts, _) = Request::builder()
            .uri("/fictional-private-path")
            .body(Body::empty())
            .expect("request")
            .into_parts();
        let error = match Path::<operation::Id>::from_request_parts(&mut parts, &()).await {
            Ok(_) => panic!("missing route parameters"),
            Err(error) => error,
        };
        assert_eq!(format!("{error:?}"), "request path extraction failed");
        let response = tracing::dispatcher::with_default(&dispatch, || error.into_response());
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.expect("body"),
            r#"{"code":"internal_error"}"#
        );

        let router = Router::new()
            .route("/{id}", get(inspect))
            .route("/{first}/{second}", get(inspect))
            .with_state(dispatch);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("server");
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("client");
        for (path, status, body) in [
            ("fictional-private-path", 400, r#"{"code":"invalid_input"}"#),
            ("%FF", 400, r#"{"code":"invalid_input"}"#),
            (
                "fictional-private-path/extra",
                500,
                r#"{"code":"internal_error"}"#,
            ),
            (
                "550e8400-e29b-41d4-a716-446655440000",
                200,
                "550e8400-e29b-41d4-a716-446655440000",
            ),
        ] {
            let response = client
                .get(format!("http://{address}/{path}"))
                .send()
                .await
                .expect("path request");
            assert_eq!(response.status(), status);
            assert_eq!(response.text().await.expect("body"), body);
        }
        server.abort();
        assert!(server.await.expect_err("cancelled server").is_cancelled());
        log.seek(SeekFrom::Start(0)).expect("rewind log");
        let mut text = String::new();
        log.read_to_string(&mut text).expect("log");
        assert!(!text.contains("fictional-private"));
        let events: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("event"))
            .collect();
        assert_eq!(events.len(), 4);
        for (event, (level, kind)) in events.iter().zip([
            ("ERROR", "internal"),
            ("INFO", "invalid_input"),
            ("INFO", "invalid_input"),
            ("ERROR", "internal"),
        ]) {
            assert_eq!(event["level"], level);
            assert_eq!(event["fields"]["stage"], "path_input");
            assert_eq!(event["fields"]["kind"], kind);
            assert_eq!(event["fields"]["error"], "request path extraction failed");
        }
    }
}
