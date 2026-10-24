//! Axum JSON extraction with application rejections and default responses.

use std::fmt;

use axum::{
    extract::{FromRequest, rejection::JsonRejection},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

use crate::response::{Common, Failure};

#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(Error))]
pub(crate) struct Json<T>(pub(crate) T);

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

#[derive(Error)]
#[error("invalid request JSON")]
pub(crate) struct Error(#[from] JsonRejection);

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        tracing::info!(stage = "json_input", kind = "invalid_input", error = ?self);
        (
            self.0.status(),
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
    use axum::{
        body::{Body, to_bytes},
        extract::{FromRequest, Request},
        http::StatusCode,
        response::IntoResponse,
    };

    use crate::json::Json;

    #[tokio::test]
    async fn syntax_and_shape_errors_keep_request_values_private() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            value: String,
        }
        for (body, status) in [
            (
                r#"{"value":"fictional-private-material""#,
                StatusCode::BAD_REQUEST,
            ),
            (r#"{"value":null}"#, StatusCode::UNPROCESSABLE_ENTITY),
            (
                r#"{"value":"first","value":"fictional-private-material"}"#,
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let request = Request::builder()
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("request");
            let error = match Json::<Input>::from_request(request, &()).await {
                Ok(_) => panic!("invalid shape"),
                Err(error) => error,
            };
            assert!(!format!("{error:?}").contains("fictional-private-material"));
            let response = error.into_response();
            assert_eq!(response.status(), status);
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.expect("body"),
                r#"{"code":"invalid_input"}"#
            );
        }
        for content_type in [
            "Application/JSON; charset=utf-8",
            "application/problem+json",
        ] {
            for body in [r#"{"value":"accepted"}"#, r#"["accepted"]"#] {
                let request = Request::builder()
                    .header("content-type", content_type)
                    .body(Body::from(body))
                    .expect("request");
                let Json(input) = Json::<Input>::from_request(request, &())
                    .await
                    .expect("JSON media type and Serde input");
                assert_eq!(input.value, "accepted");
            }
        }
    }
}
