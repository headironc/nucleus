//! Axum query extraction with application rejections.

use std::fmt;

use axum::{
    extract::{FromRequestParts, rejection::QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

use crate::{
    json::Json,
    response::{Common, Failure},
};

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(Error))]
pub(crate) struct Query<T>(pub(crate) T);

#[derive(Error)]
#[error("invalid request query")]
pub(crate) struct Error(#[from] QueryRejection);

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        tracing::info!(stage = "query_input", kind = "invalid_input", error = ?self);
        (
            StatusCode::BAD_REQUEST,
            Json(Failure {
                code: Common::InvalidInput,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::io::{Read, Seek, SeekFrom};

    use axum::{
        body::{Body, to_bytes},
        extract::FromRequestParts,
        http::{Request, StatusCode},
        response::IntoResponse,
    };

    use crate::query::Query;

    #[tokio::test]
    async fn shape_and_value_errors_keep_request_values_private() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            value: u64,
        }
        let mut log = tempfile::tempfile().expect("log file");
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_writer(log.try_clone().expect("log writer"))
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        for query in [
            "value=fictional-private-query",
            "value=1&value=fictional-private-query",
            "value=1&fictional-private-field=fictional-private-query",
            "",
        ] {
            let (mut parts, _) = Request::builder()
                .uri(format!("/?{query}"))
                .body(Body::empty())
                .expect("request")
                .into_parts();
            let error = match Query::<Input>::from_request_parts(&mut parts, &()).await {
                Ok(_) => panic!("invalid query"),
                Err(error) => error,
            };
            assert_eq!(format!("{error:?}"), "invalid request query");
            let response = tracing::dispatcher::with_default(&dispatch, || error.into_response());
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.expect("body"),
                r#"{"code":"invalid_input"}"#
            );
        }
        let (mut parts, _) = Request::builder()
            .uri("/?value=42")
            .body(Body::empty())
            .expect("request")
            .into_parts();
        let Query(input) = Query::<Input>::from_request_parts(&mut parts, &())
            .await
            .expect("valid query");
        assert_eq!(input.value, 42);
        log.seek(SeekFrom::Start(0)).expect("rewind log");
        let mut text = String::new();
        log.read_to_string(&mut text).expect("log");
        assert!(!text.contains("fictional-private"));
        let events: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("event"))
            .collect();
        assert_eq!(events.len(), 4);
        for event in events {
            assert_eq!(event["level"], "INFO");
            assert_eq!(event["fields"]["stage"], "query_input");
            assert_eq!(event["fields"]["kind"], "invalid_input");
            assert_eq!(event["fields"]["error"], "invalid request query");
        }
    }
}
