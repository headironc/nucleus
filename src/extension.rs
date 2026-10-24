//! Required request facts have already been established by middleware.

use axum::{
    extract::{FromRequestParts, rejection::ExtensionRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

use crate::{
    json::Json,
    response::{Common, Failure},
};

#[derive(FromRequestParts)]
#[from_request(via(axum::Extension), rejection(Error))]
pub(crate) struct Extension<T>(pub(crate) T);

#[derive(Debug, Error)]
#[error("required request extension is missing")]
pub(crate) struct Error(#[from] ExtensionRejection);

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        tracing::error!(stage = "request_extension", kind = "internal", error = ?self);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(Failure {
                code: Common::InternalError,
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
        extract::FromRequestParts,
        http::{Request, StatusCode},
        response::IntoResponse,
    };
    use domain::identity::operation;

    use crate::extension::Extension;

    #[tokio::test]
    async fn reads_only_established_facts_and_reports_missing_wiring_as_internal() {
        let (mut parts, _) = Request::builder()
            .header("operation-id", "550e8400-e29b-41d4-a716-446655440000")
            .body(Body::empty())
            .expect("request")
            .into_parts();
        let response = match Extension::<operation::Id>::from_request_parts(&mut parts, &()).await {
            Ok(_) => panic!("a header must not substitute for middleware"),
            Err(error) => error.into_response(),
        };
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.expect("body"),
            r#"{"code":"internal_error"}"#
        );
        let id: operation::Id = "550e8400-e29b-41d4-a716-446655440000"
            .parse()
            .expect("UUID");
        parts.extensions.insert(id);
        parts
            .headers
            .insert("operation-id", "invalid".parse().expect("header"));
        let Extension(actual) = Extension::<operation::Id>::from_request_parts(&mut parts, &())
            .await
            .expect("already validated fact");
        assert_eq!(actual, id);
    }
}
