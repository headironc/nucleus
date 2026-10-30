//! Map authentication outcomes while preserving unknown and failed-termination classifications.

use std::time::Duration;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use domain::identity::{authentication, operation};
use serde::Serialize;
use strum::IntoStaticStr;
use thiserror::Error;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{RefOr, Schema},
};

use crate::{
    json::Json,
    response::{self, Common},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr, Serialize)]
#[strum(serialize_all = "snake_case")]
#[serde(into = "&'static str")]
pub(super) enum Code {
    #[strum(transparent)]
    Common(Common),
    VerificationInvalid,
    VerificationRequired,
    MobileUnsupported,
    TargetChanged,
    TargetUnavailable,
    StateConflict,
}

impl From<Common> for Code {
    fn from(code: Common) -> Self {
        Self::Common(code)
    }
}

impl response::AsCode for Code {}

impl PartialSchema for Code {
    fn schema() -> RefOr<Schema> {
        Common::schema()
    }
}

impl ToSchema for Code {}

#[derive(Debug, Error)]
pub(super) enum Error {
    #[error("authentication operation failed")]
    Operation {
        #[source]
        cause: authentication::Error,
        status: StatusCode,
        code: Code,
    },
    #[error("C1 message quota is exhausted")]
    Limited {
        #[source]
        cause: authentication::Error,
        wait: Duration,
    },
    #[error("authentication caller wait expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("C1 retry delay is unavailable")]
    Retry {
        quota: Box<authentication::Error>,
        #[source]
        cause: domain::startup::Error,
    },
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::Operation { status, code, .. } => (status, code),
            Self::Limited { wait, .. } => return response::limited(wait),
            Self::Timeout(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Common::OutcomeUnknown.into(),
            ),
            Self::Retry { .. } => (
                StatusCode::SERVICE_UNAVAILABLE,
                Common::DependencyUnavailable.into(),
            ),
        };
        (status, Json(response::Failure { code })).into_response()
    }
}

/// Prepare response data explicitly; synchronous error conversions never sample time.
pub(super) async fn prepare(
    cause: authentication::Error,
    service: &domain::startup::Services,
    operation_id: operation::Id,
) -> Error {
    let failure = cause.failure();
    let authentication::Failure::Quota { retry_at } = failure else {
        let (status, code) = classify(failure);
        return Error::Operation {
            cause,
            status,
            code,
        };
    };
    let error = match service.retry_after(retry_at).await {
        Ok(wait) => return Error::Limited { cause, wait },
        Err(error) => error,
    };
    tracing::error!(%operation_id, stage = "quota_response", kind = "unavailable", error = ?error);
    Error::Retry {
        quota: Box::new(cause),
        cause: error,
    }
}

fn classify(failure: authentication::Failure) -> (StatusCode, Code) {
    use authentication::Failure;

    match failure {
        Failure::Proof => (
            StatusCode::UNAUTHORIZED,
            Common::AuthenticationInvalid.into(),
        ),
        Failure::Denied => (StatusCode::FORBIDDEN, Common::PermissionDenied.into()),
        Failure::MobileUnsupported => (StatusCode::FORBIDDEN, Code::MobileUnsupported),
        Failure::Absent => (StatusCode::NOT_FOUND, Code::TargetUnavailable),
        Failure::Mismatch => (StatusCode::CONFLICT, Common::RequestMismatch.into()),
        Failure::TargetChanged => (StatusCode::CONFLICT, Code::TargetChanged),
        Failure::Conflict => (StatusCode::CONFLICT, Code::StateConflict),
        Failure::Rejected | Failure::CodeInvalid => {
            (StatusCode::FORBIDDEN, Code::VerificationInvalid)
        }
        Failure::Suspended => (StatusCode::FORBIDDEN, Common::PermissionDenied.into()),
        Failure::Verification => (StatusCode::GONE, Code::VerificationRequired),
        Failure::Quota { .. } => (StatusCode::TOO_MANY_REQUESTS, Common::RateLimited.into()),
        Failure::Unknown => (
            StatusCode::SERVICE_UNAVAILABLE,
            Common::OutcomeUnknown.into(),
        ),
        Failure::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Common::DependencyUnavailable.into(),
        ),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use axum::{body::to_bytes, http::StatusCode, response::IntoResponse};
    use domain::identity::authentication::Error;
    use utoipa::PartialSchema;

    use crate::{
        identity::authentication::error::{self, classify},
        response::{self, Common},
    };

    #[tokio::test]
    async fn responses_keep_causes_private_and_omit_request_identity() {
        let retry_at = chrono::DateTime::from_timestamp(1_800_000_000, 0).expect("fixture time");
        let timeout = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
            .await
            .expect_err("caller wait expires");
        for (error, status, code) in [
            (
                error::Error::Operation {
                    cause: Error::Proof,
                    status: StatusCode::UNAUTHORIZED,
                    code: Common::AuthenticationInvalid.into(),
                },
                StatusCode::UNAUTHORIZED,
                "authentication_invalid",
            ),
            (
                error::Error::Limited {
                    cause: Error::Quota { retry_at },
                    wait: Duration::from_millis(1001),
                },
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
            ),
            (
                error::Error::Timeout(timeout),
                StatusCode::SERVICE_UNAVAILABLE,
                "outcome_unknown",
            ),
            (
                error::Error::Retry {
                    quota: Box::new(Error::Quota { retry_at }),
                    cause: domain::startup::Error::Work(domain::runtime::work::Error::Closed),
                },
                StatusCode::SERVICE_UNAVAILABLE,
                "dependency_unavailable",
            ),
        ] {
            assert!(std::error::Error::source(&error).is_some());
            let response = error.into_response();
            assert_eq!(response.status(), status);
            if status == StatusCode::TOO_MANY_REQUESTS {
                assert_eq!(response.headers()["retry-after"], "2");
            } else {
                assert!(!response.headers().contains_key("retry-after"));
            }
            let bytes = to_bytes(response.into_body(), 1024)
                .await
                .expect("error body");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).expect("JSON"),
                serde_json::json!({"code":code})
            );
        }
    }

    #[test]
    fn classifies_business_refusals_and_preserves_recovery_meaning() {
        assert_eq!(
            serde_json::to_value(error::Code::schema()).expect("authentication code schema"),
            serde_json::json!({"type":"string"})
        );
        assert_eq!(
            serde_json::to_value(response::Failure::<error::Code>::schema())
                .expect("authentication failure schema"),
            serde_json::to_value(response::Failure::<Common>::schema())
                .expect("common failure schema")
        );
        for (cause, status, code) in [
            (
                Error::Proof,
                StatusCode::UNAUTHORIZED,
                "authentication_invalid",
            ),
            (Error::Mismatch, StatusCode::CONFLICT, "request_mismatch"),
            (
                Error::Occupied,
                StatusCode::FORBIDDEN,
                "verification_invalid",
            ),
            (
                Error::Rejected,
                StatusCode::FORBIDDEN,
                "verification_invalid",
            ),
            (
                Error::Verification,
                StatusCode::GONE,
                "verification_required",
            ),
            (
                Error::Closed,
                StatusCode::SERVICE_UNAVAILABLE,
                "dependency_unavailable",
            ),
        ] {
            let (actual_status, actual_code) = classify(cause.failure());
            assert_eq!(actual_status, status);
            assert_eq!(
                serde_json::to_value(actual_code).expect("public code"),
                code
            );
        }
        let retry_at = chrono::DateTime::from_timestamp(1_800_000_000, 0).expect("fixture time");
        assert_eq!(
            classify(
                Error::NotCommitted {
                    cause: Box::new(Error::Quota { retry_at })
                }
                .failure()
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                error::Code::Common(Common::RateLimited)
            )
        );
    }

    #[tokio::test]
    async fn rollback_timeout_keeps_the_technical_response_without_retry_after() {
        let elapsed = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
            .await
            .expect_err("rollback deadline");
        let cause = Error::Rollback {
            cause: Box::new(Error::Quota {
                retry_at: chrono::DateTime::from_timestamp(1_800_000_000, 0).expect("fixture time"),
            }),
            rollback: domain::database::transaction::EndError::RollbackTimeout(elapsed),
        };
        let (status, code) = classify(cause.failure());
        let response = error::Error::Operation {
            cause,
            status,
            code,
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!response.headers().contains_key("retry-after"));
        let body = to_bytes(response.into_body(), 1024)
            .await
            .expect("safe response");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).expect("JSON"),
            serde_json::json!({"code":"dependency_unavailable"})
        );
    }

    #[tokio::test]
    async fn failed_termination_never_becomes_an_ordinary_refusal_or_rate_limit() {
        let retry_at = chrono::DateTime::from_timestamp(1_800_000_000, 0).expect("fixture time");
        for (cause, code) in [
            (Error::Proof, "dependency_unavailable"),
            (Error::Quota { retry_at }, "dependency_unavailable"),
            (
                Error::Unknown {
                    cause: Box::new(Error::Busy),
                    recovery: Box::new(Error::Closed),
                },
                "outcome_unknown",
            ),
            (
                Error::MessageConclusion {
                    cause: Box::new(Error::Closed),
                    persistence: Box::new(Error::Busy),
                },
                "outcome_unknown",
            ),
        ] {
            let allowance = tokio::sync::Semaphore::new(0);
            allowance.close();
            let termination = platform::database::Error::RecoveryClosed(
                allowance
                    .acquire()
                    .await
                    .expect_err("closed fixture allowance"),
            );
            let error = Error::NotCommitted {
                cause: Box::new(Error::Combined {
                    cause: Box::new(cause),
                    termination,
                }),
            };
            let (status, actual_code) = classify(error.failure());
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                serde_json::to_value(actual_code).expect("public code"),
                code
            );
        }
    }
}
