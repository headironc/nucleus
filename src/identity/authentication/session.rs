//! Creating a website SMS session exposes its token only as the original secure cookie.

use std::time::Duration;

use axum::{
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::identity::{authentication, message, mobile::Mobile, operation, registration, request};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use utoipa::ToSchema;
use zeroize::Zeroizing;

use crate::{
    extension::Extension,
    identity::{Context, authentication::error, session},
    json::Json,
    response,
};

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = session::Input)]
pub(super) struct Input {
    #[schema(
        value_type = String,
        min_length = 43,
        max_length = 43,
        pattern = "^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$"
    )]
    secret: request::Secret,
    credential: Credential,
}

#[derive(Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[schema(as = session::Credential)]
enum Credential {
    // utoipa's internally tagged variants do not inherit deny_unknown_fields.
    #[schema(max_properties = 4)]
    Message {
        id: message::Id,
        #[schema(value_type = String, min_length = 11, max_length = 11, pattern = "^(?:13[0-9]|14[01456879]|15[0-35-9]|16[2567]|17[0-8]|18[0-9]|19[0-35-9])[0-9]{8}$")]
        mobile: Mobile,
        #[schema(value_type = String, pattern = "^[0-9]{6}$")]
        code: message::Code,
    },
}

impl Input {
    fn into_command(self, operation_id: operation::Id) -> authentication::session::Input {
        let Credential::Message { id, mobile, code } = self.credential;
        authentication::session::Input::new(operation_id, self.secret, id, mobile, code)
    }
}

#[derive(Serialize, ToSchema)]
#[serde(untagged)]
#[schema(as = session::Receipt)]
enum Receipt {
    #[schema(max_properties = 2)]
    Session {
        code: &'static str,
        session: session::Session,
    },
    #[schema(max_properties = 2)]
    Registration {
        code: &'static str,
        registration: Registration,
    },
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = session::Registration, max_properties = 3)]
struct Registration {
    id: registration::Id,
    #[schema(
        value_type = String,
        min_length = 43,
        max_length = 43,
        pattern = "^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$"
    )]
    token: Zeroizing<String>,
    expires_at: DateTime<Utc>,
}

#[utoipa::path(post, path = "/sessions", operation_id = "createSession",
    params(("Operation-Id" = Uuid, Header, description = "Original R1 operation UUID"),
        ("Origin" = String, Header, description = "Required configured HTTPS origin")),
    request_body(content = Input, content_type = "application/json"),
    responses(
        (status = 200, description = "Original session cookie or pending registration", body = Receipt, headers(("Set-Cookie" = String, description = "Only a session result sets its original secure cookie"))),
        (status = 400, description = "invalid_input", body = response::Failure),
        (status = 401, description = "authentication_invalid", body = response::Failure),
        (status = 403, description = "verification_invalid or permission_denied", body = response::Failure),
        (status = 409, description = "request_mismatch", body = response::Failure),
        (status = 410, description = "verification_required", body = response::Failure),
        (status = 413, description = "invalid_input", body = response::Failure),
        (status = 415, description = "invalid_input", body = response::Failure),
        (status = 422, description = "invalid_input", body = response::Failure),
        (status = 429, description = "rate_limited", body = response::Failure),
        (status = 500, description = "internal_error", body = response::Failure),
        (status = 503, description = "dependency_unavailable or outcome_unknown", body = response::Failure)
    )
)]
pub(super) async fn create(
    Extension(operation_id): Extension<operation::Id>,
    State(context): State<Context>,
    Json(input): Json<Input>,
) -> Response {
    let input = input.into_command(operation_id);
    let receipt = match timeout(
        Duration::from_secs(30),
        context.services.authentication().create_session(input),
    )
    .await
    {
        Ok(Ok(receipt)) => receipt,
        Ok(Err(cause)) => {
            return error::prepare(cause, &context.services, operation_id)
                .await
                .into_response();
        }
        Err(cause) => return error::Error::Timeout(cause).into_response(),
    };
    match receipt {
        authentication::session::Receipt::Session(session) => {
            let value = match session::cookie(session.id(), session.token(), session.expires_at()) {
                Ok(value) => value,
                Err(cause) => {
                    tracing::error!(stage = "create_session_cookie", kind = "internal", error = ?cause);
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(response::Failure {
                            code: response::Common::InternalError,
                        }),
                    )
                        .into_response();
                }
            };
            (
                [(header::SET_COOKIE, value)],
                Json(Receipt::Session {
                    code: "ok",
                    session: session::Session {
                        id: session.id(),
                        expires_at: session.expires_at(),
                    },
                }),
            )
                .into_response()
        }
        authentication::session::Receipt::Registration(registration) => {
            Json(Receipt::Registration {
                code: "ok",
                registration: Registration {
                    id: registration.id(),
                    token: registration.token().text(),
                    expires_at: registration.expires_at(),
                },
            })
            .into_response()
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        extract::{FromRequest, Request},
        response::IntoResponse,
    };

    use crate::{identity::authentication::session::Input, json::Json};

    #[tokio::test]
    async fn tagged_credentials_validate_values_and_reject_unknown_fields() {
        let body = r#"{"secret":"\u0041AECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","credential":{"type":"message","id":"00000000-0000-0000-0000-000000000002","mobile":"\u00313800000000","code":"\u003001234"}}"#;
        serde_json::from_str::<Input>(body).expect("escaped text reaches controlled constructors");
        let body: serde_json::Value = serde_json::from_str(body).expect("fixture JSON");
        for (outer, field, value) in [
            (true, "transport", serde_json::json!("bearer")),
            (true, "secret", serde_json::json!("!".repeat(43))),
            (true, "secret", serde_json::json!("A".repeat(42))),
            (true, "secret", serde_json::json!("A".repeat(44))),
            (true, "secret", serde_json::json!("A".repeat(42) + "B")),
            (true, "secret", serde_json::json!("A".repeat(42) + "=")),
            (true, "secret", serde_json::Value::Null),
            (true, "credential", serde_json::Value::Null),
            (false, "extra", serde_json::json!("fictional-private-text")),
            (false, "type", serde_json::json!("website")),
            (false, "code", serde_json::json!(12345678)),
            (false, "code", serde_json::json!("12345")),
            (false, "code", serde_json::json!("1234567")),
            (false, "code", serde_json::json!("12345678")),
            (false, "code", serde_json::json!("１２３４５６")),
            (false, "mobile", serde_json::Value::Null),
            (false, "mobile", serde_json::json!("14200000000")),
            (false, "mobile", serde_json::json!("15400000000")),
            (false, "mobile", serde_json::json!("17900000000")),
            (false, "mobile", serde_json::json!("19400000000")),
            (false, "mobile", serde_json::json!("13800000000 ")),
        ] {
            let mut body = body.clone();
            if outer {
                body[field] = value;
            } else {
                body["credential"][field] = value;
            }
            let request = Request::builder()
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request");
            let error = match Json::<Input>::from_request(request, &()).await {
                Ok(_) => panic!("unknown or malformed field"),
                Err(error) => error,
            };
            assert!(!format!("{error:?}").contains("fictional-private-text"));
            let response = error.into_response();
            assert_eq!(response.status(), 422);
            assert_eq!(
                to_bytes(response.into_body(), 1024)
                    .await
                    .expect("safe rejection"),
                r#"{"code":"invalid_input"}"#
            );
        }
    }
}
