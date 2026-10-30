//! Creating a website SMS session exposes its token only as the original secure cookie.

use std::time::Duration;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::identity::{
    authentication, message, mini, mobile::Mobile, operation, registration, request,
};
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

#[derive(ToSchema)]
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
    #[schema(max_properties = 2)]
    Mobile {
        #[schema(value_type = String, min_length = 1, max_length = 2048)]
        code: mini::Code,
    },
}

impl<'de> Deserialize<'de> for Credential {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Credential;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a message or mobile credential")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Credential, M::Error> {
                #[derive(Deserialize)]
                #[serde(field_identifier, rename_all = "snake_case")]
                enum Field {
                    Type,
                    Id,
                    Mobile,
                    Code,
                }
                let (mut kind, mut id, mut mobile, mut code) = (None, None, None, None);
                while let Some(field) = map.next_key::<Field>()? {
                    match field {
                        Field::Type => {
                            if kind.is_some() {
                                return Err(serde::de::Error::duplicate_field("type"));
                            }
                            kind = Some(map.next_value::<String>()?);
                        }
                        Field::Id => {
                            if id.is_some() {
                                return Err(serde::de::Error::duplicate_field("id"));
                            }
                            id = Some(map.next_value::<message::Id>()?);
                        }
                        Field::Mobile => {
                            if mobile.is_some() {
                                return Err(serde::de::Error::duplicate_field("mobile"));
                            }
                            mobile = Some(map.next_value::<Mobile>()?);
                        }
                        Field::Code => {
                            if code.is_some() {
                                return Err(serde::de::Error::duplicate_field("code"));
                            }
                            code = Some(map.next_value::<Zeroizing<String>>()?);
                        }
                    }
                }
                let code = code.ok_or_else(|| serde::de::Error::missing_field("code"))?;
                match kind.as_deref() {
                    Some("message") => Ok(Credential::Message {
                        id: id.ok_or_else(|| serde::de::Error::missing_field("id"))?,
                        mobile: mobile.ok_or_else(|| serde::de::Error::missing_field("mobile"))?,
                        code: code.parse().map_err(serde::de::Error::custom)?,
                    }),
                    Some("mobile") if id.is_none() && mobile.is_none() => Ok(Credential::Mobile {
                        code: code.try_into().map_err(serde::de::Error::custom)?,
                    }),
                    _ => Err(serde::de::Error::custom("invalid credential shape")),
                }
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

#[derive(Serialize, ToSchema)]
#[serde(untagged)]
#[schema(as = session::Receipt)]
enum Receipt {
    #[schema(max_properties = 2)]
    Session {
        code: &'static str,
        session: session::Delivery,
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
    description = "Message credentials require the configured website Origin and deliver a cookie. Mobile credentials contain a getPhoneNumber code, use the configured mini-program identity, require no website Origin, and deliver an opaque Bearer token. Missing mini-program configuration returns dependency_unavailable without claiming or exchanging the code.",
    params(("Operation-Id" = Uuid, Header, description = "Original R1 operation UUID")),
    request_body(content = Input, content_type = "application/json"),
    responses(
        (status = 200, description = "Source-specific original session or pending registration", body = Receipt, headers(("Set-Cookie" = String, description = "Only a website session result sets its original secure cookie"))),
        (status = 400, description = "invalid_input", body = response::Failure),
        (status = 401, description = "authentication_invalid", body = response::Failure),
        (status = 403, description = "verification_invalid, permission_denied or mobile_unsupported", body = response::Failure),
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
    headers: HeaderMap,
    Json(input): Json<Input>,
) -> Response {
    let bearer = matches!(&input.credential, Credential::Mobile { .. });
    if !bearer && let Err(error) = crate::identity::request::origin(&headers, &context) {
        return error.into_response();
    }
    let receipt = match timeout(Duration::from_secs(30), async {
        match input.credential {
            Credential::Message { id, mobile, code } => {
                context
                    .services
                    .authentication()
                    .create_session(authentication::session::Input::new(
                        operation_id,
                        input.secret,
                        id,
                        mobile,
                        code,
                    ))
                    .await
            }
            Credential::Mobile { code } => {
                context
                    .services
                    .authentication()
                    .create_mobile_session(authentication::session::mobile::Input::new(
                        operation_id,
                        input.secret,
                        code,
                    ))
                    .await
            }
        }
    })
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
            if bearer {
                return Json(Receipt::Session {
                    code: "ok",
                    session: session::Delivery::Mini(session::Mini {
                        id: session.id(),
                        token: session.token().text(),
                        expires_at: session.expires_at(),
                    }),
                })
                .into_response();
            }
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
                    session: session::Delivery::Website(session::Session {
                        id: session.id(),
                        expires_at: session.expires_at(),
                    }),
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
