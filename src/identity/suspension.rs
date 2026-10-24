//! Supervised website suspension, resumption and state history.

use axum::{
    extract::State,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::{
    identity::{self, account, current::Actor, operation, suspension::history},
    pagination::Pagination,
    suspension,
};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    extension::Extension,
    identity::{Context, account::target, current, person::Identity},
    json::Json,
    query::Query,
    response::Failure,
};

pub(super) fn router(context: &Context) -> OpenApiRouter<Context> {
    OpenApiRouter::new()
        .routes(routes!(suspend, history))
        .routes(routes!(resume))
        .route_layer(middleware::from_fn_with_state(
            context.clone(),
            current::authenticate,
        ))
        .route_layer(middleware::from_fn(target))
}

#[derive(Serialize, ToSchema)]
#[schema(as = suspension::Receipt)]
struct Receipt {
    code: &'static str,
    suspension: identity::suspension::Suspension,
}

#[derive(Serialize, ToSchema)]
#[schema(as = suspension::ResumptionReceipt)]
struct ResumptionReceipt {
    code: &'static str,
    resumption: identity::suspension::Resumption,
}

#[derive(Serialize, ToSchema)]
#[schema(as = suspension::history::List)]
struct History<'a> {
    code: &'static str,
    #[schema(minimum = 0)]
    count: i64,
    suspensions: Vec<Suspension<'a>>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = suspension::history::Suspension)]
struct Suspension<'a> {
    id: identity::suspension::Id,
    subject: Identity<'a>,
    operator: Identity<'a>,
    sequence: i64,
    #[schema(value_type = String, format = DateTime)]
    suspended_at: DateTime<Utc>,
    operation_id: operation::Id,
    #[schema(required)]
    resumption: Option<Resumption<'a>>,
}

impl<'a> From<&'a history::Suspension> for Suspension<'a> {
    fn from(value: &'a history::Suspension) -> Self {
        Self {
            id: value.id(),
            subject: value.subject().into(),
            operator: value.operator().into(),
            sequence: value.sequence(),
            suspended_at: value.suspended_at(),
            operation_id: value.operation_id(),
            resumption: value.resumption().map(Resumption::from),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = suspension::history::Resumption)]
struct Resumption<'a> {
    subject: Identity<'a>,
    operator: Identity<'a>,
    sequence: i64,
    #[schema(value_type = String, format = DateTime)]
    resumed_at: DateTime<Utc>,
    operation_id: operation::Id,
}

impl<'a> From<&'a history::Resumption> for Resumption<'a> {
    fn from(value: &'a history::Resumption) -> Self {
        Self {
            subject: value.subject().into(),
            operator: value.operator().into(),
            sequence: value.sequence(),
            resumed_at: value.resumed_at(),
            operation_id: value.operation_id(),
        }
    }
}

#[utoipa::path(get, path = "/accounts/{id}/suspensions", operation_id = "listAccountSuspensions",
    description = "Requires a current valid website session, institution.accounts and another uncancelled account with a strictly lower institutional level. Suspended targets remain readable. Returns permanent suspensions ordered by Account sequence descending, then ID descending, with their optional resumptions and each action's fixed masked identities. Authorization, target, count and complete page share one three-second read-only snapshot. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin.",
    params(("id" = account::Id, Path, description = "Current target account"), ("Operation-Id" = Uuid, Header, description = "Required request UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"), ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20")),
    responses((status = 200, description = "Complete account state history page", body = History), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn history(
    Extension(actor): Extension<Actor>,
    Extension(account_id): Extension<account::Id>,
    State(context): State<Context>,
    Query(pagination): Query<Pagination>,
) -> Response {
    crate::route::query(
        "suspension_history",
        context
            .services
            .suspension()
            .history(&actor, account_id, pagination),
        |list| {
            Json(History {
                code: "ok",
                count: list.count(),
                suspensions: list.suspensions().iter().map(Suspension::from).collect(),
            })
            .into_response()
        },
    )
    .await
}

#[utoipa::path(post, path = "/accounts/{id}/suspensions", operation_id = "suspendAccount",
    description = "Suspends another strictly lower institutional account and revokes every retained unrevoked session atomically. Requires a current valid website session, institution.accounts and exact Origin. Any supplied Sec-Fetch-Site must be same-origin. The same operation and confirmed input return the permanent original suspension while current authentication and permission still hold. The administrator cookie is unchanged.",
    params(("id" = account::Id, Path, description = "Confirmed target account"), ("Operation-Id" = Uuid, Header, description = "Original suspension operation UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = suspension::Input, content_type = "application/json"),
    responses((status = 201, description = "Permanent original suspension", body = Receipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "request_mismatch, target_changed or state_conflict", body = Failure), (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn suspend(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    Extension(account_id): Extension<account::Id>,
    State(context): State<Context>,
    Json(input): Json<suspension::Input>,
) -> Response {
    crate::route::command(
        context
            .services
            .suspension()
            .suspend(actor, operation_id, account_id, input),
        |suspension| {
            (
                StatusCode::CREATED,
                Json(Receipt {
                    code: "ok",
                    suspension,
                }),
            )
                .into_response()
        },
    )
    .await
}

#[utoipa::path(post, path = "/accounts/{id}/resumptions", operation_id = "resumeAccount",
    description = "Resumes only the explicitly confirmed open suspension of another strictly lower institutional account. Requires a current valid website session, institution.accounts and exact Origin. Any supplied Sec-Fetch-Site must be same-origin. Old sessions remain revoked. Replaying the original operation while current authentication and permission still hold returns its permanent result without clearing any later suspension. The administrator cookie is unchanged.",
    params(("id" = account::Id, Path, description = "Confirmed target account"), ("Operation-Id" = Uuid, Header, description = "Original resumption operation UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = suspension::Resume, content_type = "application/json"),
    responses((status = 200, description = "Permanent original resumption", body = ResumptionReceipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "request_mismatch, target_changed or state_conflict", body = Failure), (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn resume(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    Extension(account_id): Extension<account::Id>,
    State(context): State<Context>,
    Json(input): Json<suspension::Resume>,
) -> Response {
    crate::route::command(
        context
            .services
            .suspension()
            .resume(actor, operation_id, account_id, input),
        |resumption| {
            Json(ResumptionReceipt {
                code: "ok",
                resumption,
            })
            .into_response()
        },
    )
    .await
}
