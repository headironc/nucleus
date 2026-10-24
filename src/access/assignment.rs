//! Confirm, replace and read permanent Workforce role assignments.

use std::time::Duration;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::{
    access::{Entry, assignment, grant},
    identity::{current::Actor, mobile::Mobile, operation, person},
    pagination::Pagination,
};
use serde::{Deserialize, Serialize};
use strum::IntoStaticStr;
use tokio::time::timeout;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{RefOr, Schema},
};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    access::Role,
    extension::Extension,
    identity::{self, account::Subject, person::Identity},
    json::Json,
    query::Query,
    response::{Common, Failure},
};

pub(super) fn router(
    service: assignment::Service,
    authentication: identity::Context,
) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(candidate))
        .routes(routes!(assign, list))
        .route_layer(middleware::from_fn_with_state(
            authentication,
            identity::current::authenticate,
        ))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(service)
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = assignment::Search)]
struct Search {
    entry: Entry,
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: Mobile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = assignment::Candidate)]
struct Candidate<'a> {
    code: &'static str,
    identity: Subject<'a>,
    grant_id: grant::Id,
    assignment_id: assignment::Id,
    role: Role<'a>,
    count: i64,
    roles: Vec<Role<'a>>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = assignment::Assignment)]
pub(super) struct Assignment<'a> {
    id: assignment::Id,
    grant_id: grant::Id,
    subject: Identity<'a>,
    operator: Identity<'a>,
    role: Role<'a>,
    sequence: i64,
    assigned_at: DateTime<Utc>,
    operation_id: operation::Id,
}

impl<'a> From<&'a assignment::Assignment> for Assignment<'a> {
    fn from(assignment: &'a assignment::Assignment) -> Self {
        Self {
            id: assignment.id(),
            grant_id: assignment.grant_id(),
            subject: Identity::from(assignment.subject()),
            operator: Identity::from(assignment.operator()),
            role: Role::from(assignment.role()),
            sequence: assignment.sequence(),
            assigned_at: assignment.assigned_at(),
            operation_id: assignment.operation_id(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = assignment::Receipt)]
struct Receipt<'a> {
    code: &'static str,
    assignment: Assignment<'a>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = assignment::List)]
struct List<'a> {
    code: &'static str,
    count: i64,
    assignments: Vec<Assignment<'a>>,
}

#[utoipa::path(get, path = "/workforce/assignments", operation_id = "listWorkforceAssignments",
    description = "List permanent Workforce assignments in time and ID order, including person-filtered history. Contract: docs/access/roles.md#指派.",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20"),
        ("personId" = Option<person::Id>, Query, description = "Optional permanent person UUID"),
        ("operationId" = Option<operation::Id>, Query, description = "Optional original operation UUID across initial and replacement families")),
    responses((status = 200, description = "Complete permanent assignment page", body = List),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn list(
    Extension(actor): Extension<Actor>,
    State(service): State<assignment::Service>,
    Query(filter): Query<assignment::Filter>,
) -> Response {
    match timeout(Duration::from_secs(30), service.list(&actor, filter)).await {
        Ok(Ok(list)) => Json(List {
            code: "ok",
            count: list.count(),
            assignments: list.assignments().iter().map(Assignment::from).collect(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == assignment::Failure::Unavailable {
                tracing::error!(stage = "assignment_history", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "assignment_history", kind = "timeout", error = ?error);
            failed(assignment::Failure::Unavailable)
        }
    }
}

#[utoipa::path(post, path = "/workforce/assignments/candidates", operation_id = "workforceAssignmentCandidate",
    description = "Confirm a Workforce assignment target and eligible roles through the selected authority. Contract: docs/access/roles.md#指派.",
    params(("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1; offset must fit PostgreSQL bigint"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20"),
        ("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = Search, content_type = "application/json"),
    responses((status = 200, description = "Confirmed member and eligible roles", body = Candidate),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn candidate(
    Extension(actor): Extension<Actor>,
    State(service): State<assignment::Service>,
    Query(pagination): Query<Pagination>,
    Json(input): Json<Search>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.candidate(&actor, input.entry, input.mobile, pagination),
    )
    .await
    {
        Ok(Ok(candidate)) => Json(Candidate {
            code: "ok",
            identity: Subject::from(candidate.identity()),
            grant_id: candidate.grant_id(),
            assignment_id: candidate.assignment_id(),
            role: Role::from(candidate.role()),
            count: candidate.count(),
            roles: candidate.roles().iter().map(Role::from).collect(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == assignment::Failure::Unavailable {
                tracing::error!(stage = "assignment_candidate", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "assignment_candidate", kind = "timeout", error = ?error);
            failed(assignment::Failure::Unavailable)
        }
    }
}

#[utoipa::path(post, path = "/workforce/assignments", operation_id = "assignWorkforceRole",
    description = "Replace the confirmed Workforce role and retain the permanent original assignment. Contract: docs/access/roles.md#指派.",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Original assignment UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = assignment::Input, content_type = "application/json"),
    responses((status = 201, description = "Original assignment with fixed masked identities and role", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "request_mismatch, target_changed, role_changed or state_conflict", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn assign(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<assignment::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<assignment::Input>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.assign(actor, operation_id, input),
    )
    .await
    {
        Ok(Ok(assignment)) => (
            StatusCode::CREATED,
            Json(Receipt {
                code: "ok",
                assignment: Assignment::from(&assignment),
            }),
        )
            .into_response(),
        Ok(Err(error)) => failed(error.failure()),
        Err(_) => failed(assignment::Failure::Unknown),
    }
}

#[derive(Clone, Copy, IntoStaticStr, Serialize)]
#[strum(serialize_all = "snake_case")]
#[serde(into = "&'static str")]
enum Code {
    #[strum(transparent)]
    Common(Common),
    TargetUnavailable,
    TargetChanged,
    RoleChanged,
    StateConflict,
}

impl crate::response::AsCode for Code {}
impl PartialSchema for Code {
    fn schema() -> RefOr<Schema> {
        Common::schema()
    }
}
impl ToSchema for Code {}

fn failed(failure: assignment::Failure) -> Response {
    let (status, code) = match failure {
        assignment::Failure::Invalid => (
            StatusCode::UNAUTHORIZED,
            Code::Common(Common::AuthenticationInvalid),
        ),
        assignment::Failure::Denied => (
            StatusCode::FORBIDDEN,
            Code::Common(Common::PermissionDenied),
        ),
        assignment::Failure::Mismatch => {
            (StatusCode::CONFLICT, Code::Common(Common::RequestMismatch))
        }
        assignment::Failure::Absent => (StatusCode::NOT_FOUND, Code::TargetUnavailable),
        assignment::Failure::TargetChanged => (StatusCode::CONFLICT, Code::TargetChanged),
        assignment::Failure::RoleChanged => (StatusCode::CONFLICT, Code::RoleChanged),
        assignment::Failure::Unchanged => (StatusCode::CONFLICT, Code::StateConflict),
        assignment::Failure::Busy => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "1")],
                Json(Failure {
                    code: Common::RateLimited,
                }),
            )
                .into_response();
        }
        assignment::Failure::Worker => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Code::Common(Common::InternalError),
        ),
        assignment::Failure::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Code::Common(Common::DependencyUnavailable),
        ),
        assignment::Failure::Unknown => (
            StatusCode::SERVICE_UNAVAILABLE,
            Code::Common(Common::OutcomeUnknown),
        ),
    };
    (status, Json(Failure { code })).into_response()
}
