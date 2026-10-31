//! The one HTTP mapping of kernel errors, and the bounded waits for commands and queries.

use std::{future::Future, time::Duration};

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use domain::kernel::{Error, Rejection, Status};
use serde::Serialize;
use tokio::time::timeout;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{RefOr, Schema},
};

use crate::response::{AsCode, Common, Failure, limited};

/// HTTP bounds its wait; each capability owns cancellation and completion.
const WAIT: Duration = Duration::from_secs(30);

/// Work-slot exhaustion asks clients to retry after this many seconds.
const RETRY: Duration = Duration::from_secs(1);

/// A common code or a capability's own rejection code.
#[derive(Clone, Copy, Serialize)]
#[serde(transparent)]
struct Code(&'static str);

impl AsCode for Code {}

impl PartialSchema for Code {
    fn schema() -> RefOr<Schema> {
        Common::schema()
    }
}

impl ToSchema for Code {}

impl From<Common> for Code {
    fn from(code: Common) -> Self {
        Self(code.into())
    }
}

/// Waits for a command's known result and projects it. A wait that ends first leaves the
/// outcome unknown.
pub(crate) async fn command<T, R: Rejection>(
    operation: impl Future<Output = Result<T, Error<R>>>,
    project: impl FnOnce(T) -> Response,
) -> Response {
    match timeout(WAIT, operation).await {
        Ok(Ok(value)) => project(value),
        // The capability reports its final technical failure once.
        Ok(Err(error)) => failure(&error),
        Err(_) => respond(StatusCode::SERVICE_UNAVAILABLE, Common::OutcomeUnknown),
    }
}

/// Waits for a query, projects it and reports its technical failure once at this boundary.
pub(crate) async fn query<T, R: Rejection>(
    stage: &'static str,
    operation: impl Future<Output = Result<T, Error<R>>>,
    project: impl FnOnce(T) -> Response,
) -> Response {
    match timeout(WAIT, operation).await {
        Ok(Ok(value)) => project(value),
        Ok(Err(error)) => {
            if error.fault().is_some() {
                tracing::error!(stage, kind = "unavailable", error = ?error);
            }
            failure(&error)
        }
        Err(error) => {
            tracing::error!(stage, kind = "timeout", error = ?error);
            respond(
                StatusCode::SERVICE_UNAVAILABLE,
                Common::DependencyUnavailable,
            )
        }
    }
}

pub(crate) fn failure<R: Rejection>(error: &Error<R>) -> Response {
    let (status, code) = match error {
        Error::Rejected(rejection) => (
            match rejection.status() {
                Status::Forbidden => StatusCode::FORBIDDEN,
                Status::NotFound => StatusCode::NOT_FOUND,
                Status::Conflict => StatusCode::CONFLICT,
                Status::Gone => StatusCode::GONE,
                Status::Unprocessable => StatusCode::UNPROCESSABLE_ENTITY,
            },
            Code(rejection.code()),
        ),
        Error::Unauthenticated => (
            StatusCode::UNAUTHORIZED,
            Common::AuthenticationInvalid.into(),
        ),
        Error::Denied => (StatusCode::FORBIDDEN, Common::PermissionDenied.into()),
        Error::Mismatch => (StatusCode::CONFLICT, Common::RequestMismatch.into()),
        Error::Busy => return limited(RETRY),
        Error::Unavailable(_) | Error::Corrupt(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Common::DependencyUnavailable.into(),
        ),
        Error::Unknown(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Common::OutcomeUnknown.into(),
        ),
    };
    (status, Json(Failure { code })).into_response()
}

fn respond(status: StatusCode, code: Common) -> Response {
    (status, Json(Failure { code })).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use axum::{body::to_bytes, http::StatusCode};
    use domain::kernel::{Error, Rejection, Status};

    use super::failure;

    #[derive(Debug)]
    struct Taken;

    impl Rejection for Taken {
        fn status(&self) -> Status {
            Status::Conflict
        }

        fn code(&self) -> &'static str {
            "name_in_use"
        }
    }

    async fn body(response: axum::response::Response) -> serde_json::Value {
        serde_json::from_slice(
            &to_bytes(response.into_body(), 1024)
                .await
                .expect("bounded error body"),
        )
        .expect("JSON error body")
    }

    #[tokio::test]
    async fn every_kernel_error_has_one_public_mapping() {
        let busy = failure(&Error::<Taken>::Busy);
        assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(busy.headers()["retry-after"], "1");
        assert_eq!(body(busy).await, serde_json::json!({"code":"rate_limited"}));
        for (error, status, code) in [
            (Error::Rejected(Taken), StatusCode::CONFLICT, "name_in_use"),
            (
                Error::Unauthenticated,
                StatusCode::UNAUTHORIZED,
                "authentication_invalid",
            ),
            (Error::Denied, StatusCode::FORBIDDEN, "permission_denied"),
            (Error::Mismatch, StatusCode::CONFLICT, "request_mismatch"),
        ] {
            let response = failure(&error);
            assert_eq!(response.status(), status);
            assert!(response.headers().get("retry-after").is_none());
            assert_eq!(body(response).await, serde_json::json!({ "code": code }));
        }
    }
}
