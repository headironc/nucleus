//! C1 request conversion, invocation and response projection.

mod error;
mod input;
mod request;
pub(crate) mod settings;

use std::time::Duration;

use axum::{
    extract::{DefaultBodyLimit, FromRef, State},
    middleware,
};
use chrono::{DateTime, Utc};
use domain::identity::{
    authentication::{self, dispatch},
    message, operation,
};
use serde::Serialize;
use tokio::time::timeout;
use utoipa::ToSchema;
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouterExt},
    routes,
};

pub(crate) use crate::identity::authentication::settings::Settings;
use crate::{extension::Extension, identity::admission, json::Json, response, source};

#[derive(Clone)]
struct Context {
    authentication: authentication::Service,
    settings: Settings,
    source: source::Settings,
    admission: admission::Admission,
}

impl FromRef<Context> for source::Settings {
    fn from_ref(context: &Context) -> Self {
        context.source.clone()
    }
}

pub(crate) fn router(
    authentication: authentication::Service,
    settings: Settings,
    source: source::Settings,
    admission: admission::Admission,
) -> OpenApiRouter {
    let context = Context {
        authentication,
        settings,
        source,
        admission,
    };
    OpenApiRouter::new()
        .routes(routes!(send).map(|router| {
            router.route_layer(middleware::from_fn_with_state(
                context.clone(),
                request::admit,
            ))
        }))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(context)
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    #[schema(example = "ok")]
    code: &'static str,
    message: Message,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct Message {
    id: message::Id,
    delivery: Delivery,
    expires_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
enum Delivery {
    Sending,
    Sent,
    Failed,
    Unknown,
}

impl From<dispatch::Receipt> for Message {
    fn from(receipt: dispatch::Receipt) -> Self {
        Self {
            id: receipt.id(),
            expires_at: receipt.expires_at(),
            delivery: match receipt.status() {
                message::Status::Sending => Delivery::Sending,
                message::Status::Sent { .. } => Delivery::Sent,
                message::Status::Failed { .. } => Delivery::Failed,
                message::Status::Unknown { .. } => Delivery::Unknown,
            },
        }
    }
}

/// Send or recover a retained C1 using its original Operation-Id, mobile and token.
/// Requires a JSON media type, at most 8192 bytes, and an exact configured Origin
/// header. No current session is required.
#[utoipa::path(post, path = "/codes", operation_id = "sendCode",
    params(("Operation-Id" = Uuid, Header, description = "Required UUID value. Reuse it with the original input while the original records are retained."),
           ("Origin" = String, Header, description = "Required; must equal the configured HTTPS origin.")),
    request_body(content = input::Input, content_type = "application/json"),
    responses(
        (status = 200, description = "Original message. sent means supplier acceptance, not delivery.", body = Receipt),
        (status = 400, description = "invalid_input; invalid headers or JSON syntax", body = response::Failure),
        (status = 401, description = "authentication_invalid", body = response::Failure),
        (status = 403, description = "verification_invalid or permission_denied", body = response::Failure),
        (status = 409, description = "request_mismatch", body = response::Failure),
        (status = 410, description = "verification_required", body = response::Failure),
        (status = 413, description = "invalid_input; body exceeds 8192 bytes", body = response::Failure),
        (status = 415, description = "invalid_input; JSON media type required", body = response::Failure),
        (status = 422, description = "invalid_input; invalid body shape or field value", body = response::Failure),
        (status = 429, description = "rate_limited; no automatic resend", body = response::Failure, headers(("Retry-After" = u64, description = "Whole seconds, rounded up, at least one"))),
        (status = 500, description = "internal_error", body = response::Failure),
        (status = 503, description = "dependency_unavailable or outcome_unknown; retain original input", body = response::Failure)
    )
)]
async fn send(
    Extension(operation_id): Extension<operation::Id>,
    State(context): State<Context>,
    Extension(source): Extension<source::Source>,
    Json(input): Json<input::Input>,
) -> Result<Json<Receipt>, error::Error> {
    let input = input.parse(operation_id, source.address())?;
    let cause = match timeout(Duration::from_secs(30), context.authentication.send(input)).await? {
        Ok(message) => {
            return Ok(Json(Receipt {
                code: "ok",
                message: message.into(),
            }));
        }
        Err(error) => error,
    };
    Err(error::prepare(cause, &context.authentication, operation_id).await)
}
