use std::time::Duration;

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use strum::IntoStaticStr;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{RefOr, Schema},
};

pub(crate) trait AsCode: Serialize + ToSchema {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr, Serialize)]
#[strum(serialize_all = "snake_case")]
#[serde(into = "&'static str")]
pub(crate) enum Common {
    InvalidInput,
    InternalError,
    AuthenticationInvalid,
    PermissionDenied,
    ResourceNotFound,
    MethodNotAllowed,
    RequestMismatch,
    RateLimited,
    DependencyUnavailable,
    OutcomeUnknown,
}

impl AsCode for Common {}

impl PartialSchema for Common {
    fn schema() -> RefOr<Schema> {
        String::schema()
    }
}

impl ToSchema for Common {}

/// An error response with an extensible code. Clients must tolerate unknown values.
#[derive(Serialize, ToSchema)]
#[schema(as = Failure, bound = "")]
pub(crate) struct Failure<C: AsCode = Common> {
    #[schema(inline)]
    pub(crate) code: C,
}

pub(crate) fn limited(wait: Duration) -> Response {
    let seconds = wait
        .as_secs()
        .saturating_add(u64::from(wait.subsec_nanos() != 0))
        .max(1);
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, HeaderValue::from(seconds))],
        Json(Failure {
            code: Common::RateLimited,
        }),
    )
        .into_response()
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use utoipa::PartialSchema;

    use crate::response::{Common, Failure, limited};

    #[test]
    fn serializes_only_the_public_code_and_keeps_the_schema_extensible() {
        let response = Failure {
            code: Common::RateLimited,
        };
        assert_eq!(
            serde_json::to_value(response).expect("public code"),
            serde_json::json!({"code":"rate_limited"}),
        );
        let schema = serde_json::to_value(Failure::<Common>::schema()).expect("schema");
        assert_eq!(
            serde_json::to_value(Common::schema()).expect("code schema"),
            serde_json::json!({"type":"string"})
        );
        assert_eq!(schema["required"], serde_json::json!(["code"]));
        assert_eq!(schema["properties"].as_object().expect("fields").len(), 1);
        assert_eq!(schema["properties"]["code"]["type"], "string", "{schema}");
        assert!(schema["properties"]["code"].get("enum").is_none());
        assert!(schema["properties"]["code"].get("$ref").is_none());
    }

    #[test]
    fn rounds_retry_after_up_to_whole_seconds() {
        for (wait, value) in [
            (Duration::ZERO, "1"),
            (Duration::from_millis(1001), "2"),
            (Duration::from_secs(2), "2"),
        ] {
            assert_eq!(limited(wait).headers()["retry-after"], value);
        }
    }
}
