//! Website cancellation preserves the manager cookie and clears only a cancelled self session.

use axum::{
    extract::State,
    http::{StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
};
use domain::{
    cancellation,
    identity::{self, account, current::Actor, operation},
};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    extension::Extension,
    identity::{Context, account::target, current, session},
    json::Json,
    response::{Common, Failure},
    route,
};

pub(super) fn router(context: &Context) -> OpenApiRouter<Context> {
    OpenApiRouter::new()
        .routes(routes!(cancel))
        .route_layer(middleware::from_fn_with_state(
            context.clone(),
            current::authenticate,
        ))
        .merge(
            OpenApiRouter::new()
                .routes(routes!(cancel_account))
                .route_layer(middleware::from_fn_with_state(
                    context.clone(),
                    current::authenticate,
                ))
                .route_layer(middleware::from_fn(target)),
        )
}

#[derive(Serialize, ToSchema)]
#[schema(as = cancellation::Receipt)]
struct Receipt {
    code: &'static str,
    cancellation: identity::cancellation::Cancellation,
}

#[utoipa::path(post, path = "/accounts/current/cancellations", operation_id = "cancelCurrentAccount",
    description = "Cancels the confirmed current account. See docs/identity/cancellation.md for qualification, atomic consequences and recovery.",
    params(("Operation-Id" = Uuid, Header, description = "Cancellation operation UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = cancellation::Input, content_type = "application/json"),
    responses((status = 200, description = "Permanent cancellation ID and effective time", body = Receipt, headers(("Set-Cookie" = String, description = "Clears only the authenticated session cookie"))), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 409, description = "request_mismatch or target_changed", body = Failure), (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn cancel(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(context): State<Context>,
    Json(input): Json<cancellation::Input>,
) -> Response {
    let id = actor.session_id();
    route::command(
        context
            .services
            .cancellation()
            .cancel(actor, operation_id, input),
        |cancellation| {
            let cookie = match session::clear(id) {
                Ok(cookie) => cookie,
                Err(error) => {
                    tracing::error!(%operation_id, stage = "cancellation_cookie", kind = "internal", error = ?error);
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(Failure { code: Common::InternalError })).into_response();
                }
            };
            ([(header::SET_COOKIE, cookie)], Json(Receipt { code: "ok", cancellation })).into_response()
        },
    ).await
}
#[utoipa::path(post, path = "/accounts/{id}/cancellations", operation_id = "cancelAccount",
    description = "Cancels a confirmed lower-level account. See docs/identity/cancellation.md for authorization, original results and recovery.",
    params(("id" = account::Id, Path, description = "Confirmed target account"), ("Operation-Id" = Uuid, Header, description = "Original cancellation operation UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = cancellation::Management, content_type = "application/json"),
    responses((status = 200, description = "Permanent cancellation ID and effective time", body = Receipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "request_mismatch or target_changed", body = Failure), (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn cancel_account(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    Extension(account_id): Extension<account::Id>,
    State(context): State<Context>,
    Json(input): Json<cancellation::Management>,
) -> Response {
    route::command(
        context
            .services
            .cancellation()
            .cancel_account(actor, operation_id, account_id, input),
        |cancellation| {
            Json(Receipt {
                code: "ok",
                cancellation,
            })
            .into_response()
        },
    )
    .await
}
