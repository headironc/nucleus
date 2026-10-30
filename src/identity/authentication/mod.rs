//! C1 request conversion, invocation and response projection.

mod error;
mod input;
mod session;

use std::time::Duration;

use axum::{extract::State, middleware};
use chrono::{DateTime, Utc};
use domain::identity::{authentication::dispatch, current::Actor, message, operation, transfer};
use serde::Serialize;
use tokio::time::timeout;
use utoipa::ToSchema;
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouterExt},
    routes,
};

use crate::{
    extension::Extension,
    identity::{Context, current, request},
    json::Json,
    response, source,
};

pub(super) fn router(context: &Context) -> OpenApiRouter<Context> {
    OpenApiRouter::new()
        .routes(routes!(send).map(|router| {
            router.route_layer(middleware::from_fn_with_state(
                context.clone(),
                request::admit,
            ))
        }))
        .routes(routes!(session::create).map(|router| {
            router.route_layer(middleware::from_fn_with_state(
                context.clone(),
                request::admit_session,
            ))
        }))
        .routes(routes!(send_current).map(|router| {
            router.route_layer(middleware::from_fn_with_state(
                context.clone(),
                current::authenticate_code,
            ))
        }))
        .routes(routes!(send_binding).map(|router| {
            router
                .route_layer(middleware::from_fn_with_state(
                    context.clone(),
                    current::authenticate_code,
                ))
                .route_layer(middleware::from_fn(crate::identity::transfer::target))
        }))
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
    let input = input.into_command(operation_id, source.address());
    let cause = match timeout(
        Duration::from_secs(30),
        context.services.authentication().send(input),
    )
    .await?
    {
        Ok(message) => {
            return Ok(Json(Receipt {
                code: "ok",
                message: message.into(),
            }));
        }
        Err(error) => error,
    };
    Err(error::prepare(cause, &context.services, operation_id).await)
}

#[utoipa::path(post, path = "/accounts/current/codes", operation_id = "sendCurrentMobileCode",
    description = "Verify and send to the current website account phone. Replays preserve the original person and session. Authentication and mobile verification share the same sending budgets.",
    params(("Operation-Id" = Uuid, Header, description = "Original sending operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Selects the website session cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = input::Input, content_type = "application/json"),
    responses((status=200,description="Original message; sent means supplier acceptance",body=Receipt),
        (status=400,description="invalid_input",body=response::Failure),
        (status=401,description="authentication_invalid",body=response::Failure),
        (status=403,description="verification_invalid or permission_denied",body=response::Failure),
        (status=409,description="request_mismatch or target_changed",body=response::Failure),
        (status=410,description="verification_required",body=response::Failure),
        (status=413,description="invalid_input; body exceeds 8192 bytes",body=response::Failure),
        (status=415,description="invalid_input",body=response::Failure),
        (status=422,description="invalid_input",body=response::Failure),
        (status=429,description="rate_limited",body=response::Failure,headers(("Retry-After"=u64,description="Wait seconds"))),
        (status=500,description="internal_error",body=response::Failure),
        (status=503,description="dependency_unavailable or outcome_unknown",body=response::Failure)))]
async fn send_current(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(context): State<Context>,
    Extension(source): Extension<source::Source>,
    Json(input): Json<input::Input>,
) -> Result<Json<Receipt>, error::Error> {
    let input = input.into_command(operation_id, source.address());
    let cause = match timeout(
        Duration::from_secs(30),
        context.services.authentication().send_current(actor, input),
    )
    .await?
    {
        Ok(message) => {
            return Ok(Json(Receipt {
                code: "ok",
                message: message.into(),
            }));
        }
        Err(error) => error,
    };
    Err(error::prepare(cause, &context.services, operation_id).await)
}

#[utoipa::path(post, path = "/mobile-changes/{id}/codes", operation_id = "sendNewMobileCode",
    description = "Send a verification code to a different phone using the website session that established the current transfer. New sends require an unexpired latest transfer and unchanged target identity. Assisted sends and original reads require current institution.accounts and a strictly lower target; paused targets remain eligible. Phone occupancy is not disclosed. Original results remain readable without another supplier call.",
    params(("id" = transfer::Id, Path, description = "Original mobile transfer"),
        ("Operation-Id" = Uuid, Header, description = "Original sending operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Selects the website session cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = input::Input, content_type = "application/json"),
    responses((status=200,description="Original message; sent means supplier acceptance",body=Receipt),
        (status=400,description="invalid_input",body=response::Failure),
        (status=401,description="authentication_invalid",body=response::Failure),
        (status=403,description="verification_invalid or permission_denied",body=response::Failure),
        (status=409,description="request_mismatch or state_conflict",body=response::Failure),
        (status=410,description="verification_required; transfer or verification no longer applicable",body=response::Failure),
        (status=404,description="target_unavailable; assisted target is not currently manageable",body=response::Failure),
        (status=413,description="invalid_input; body exceeds 8192 bytes",body=response::Failure),
        (status=415,description="invalid_input",body=response::Failure),
        (status=422,description="invalid_input",body=response::Failure),
        (status=429,description="rate_limited",body=response::Failure,headers(("Retry-After"=u64,description="Wait seconds"))),
        (status=500,description="internal_error",body=response::Failure),
        (status=503,description="dependency_unavailable or outcome_unknown",body=response::Failure)))]
async fn send_binding(
    Extension(actor): Extension<Actor>,
    Extension(transfer_id): Extension<transfer::Id>,
    Extension(operation_id): Extension<operation::Id>,
    State(context): State<Context>,
    Extension(source): Extension<source::Source>,
    Json(input): Json<input::Input>,
) -> Result<Json<Receipt>, error::Error> {
    let input = input.into_command(operation_id, source.address());
    let cause = match timeout(
        Duration::from_secs(30),
        context
            .services
            .authentication()
            .send_binding(actor, transfer_id, input),
    )
    .await?
    {
        Ok(message) => {
            return Ok(Json(Receipt {
                code: "ok",
                message: message.into(),
            }));
        }
        Err(error) => error,
    };
    Err(error::prepare(cause, &context.services, operation_id).await)
}
