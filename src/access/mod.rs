//! Access adapters through the shared website identity boundary.

mod assignment;
mod grant;

use std::time::Duration;

use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::{
    access::{appointment, role},
    identity::{current::Actor, mobile::Mobile, operation},
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
    extension::Extension,
    identity,
    identity::{account::Subject, person::Identity},
    json::Json,
    path::Path,
    query::Query,
    response::{Common, Failure},
    route,
};

pub(crate) fn router(
    service: role::Service,
    appointments: appointment::Service,
    grants: domain::access::grant::Service,
    workforce_assignments: domain::access::assignment::Service,
    authentication: identity::Context,
) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(read))
        .routes(routes!(list))
        .routes(routes!(create))
        .routes(routes!(list_workforce, create_workforce))
        .routes(routes!(changes))
        .routes(routes!(changes_workforce))
        .route_layer(middleware::from_fn_with_state(
            authentication.clone(),
            identity::current::authenticate,
        ))
        .merge(
            OpenApiRouter::new()
                .routes(routes!(update, delete))
                .routes(routes!(update_workforce, delete_workforce))
                .route_layer(middleware::from_fn_with_state(
                    authentication.clone(),
                    identity::current::authenticate,
                ))
                .route_layer(middleware::from_fn(target)),
        )
        .layer(DefaultBodyLimit::max(8192))
        .with_state(service)
        .merge(
            OpenApiRouter::new()
                .routes(routes!(candidate))
                .routes(routes!(available))
                .routes(routes!(assign))
                .routes(routes!(assignments))
                .route_layer(middleware::from_fn_with_state(
                    authentication.clone(),
                    identity::current::authenticate,
                ))
                .layer(DefaultBodyLimit::max(8192))
                .with_state(appointments),
        )
        .merge(grant::router(grants, authentication.clone()))
        .merge(assignment::router(workforce_assignments, authentication))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = appointment::Search)]
struct Search {
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: Mobile,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = appointment::Candidate)]
struct Candidate<'a> {
    code: &'static str,
    identity: Subject<'a>,
    assignment_id: appointment::Id,
    role: Role<'a>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = appointment::Appointment)]
struct Appointment<'a> {
    id: appointment::Id,
    subject: Identity<'a>,
    operator: Identity<'a>,
    role: Role<'a>,
    sequence: i64,
    assigned_at: DateTime<Utc>,
    operation_id: operation::Id,
}

impl<'a> From<&'a appointment::Appointment> for Appointment<'a> {
    fn from(appointment: &'a appointment::Appointment) -> Self {
        Self {
            id: appointment.id(),
            subject: Identity::from(appointment.subject()),
            operator: Identity::from(appointment.operator()),
            role: Role::from(appointment.role()),
            sequence: appointment.sequence(),
            assigned_at: appointment.assigned_at(),
            operation_id: appointment.operation_id(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = appointment::Receipt)]
struct Assigned<'a> {
    code: &'static str,
    assignment: Appointment<'a>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = appointment::List)]
struct Appointments<'a> {
    code: &'static str,
    count: i64,
    assignments: Vec<Appointment<'a>>,
}

#[utoipa::path(get, path = "/assignments", operation_id = "listAssignments",
    description = "List permanent institutional appointments with fixed role definitions and masked identities. Contract: docs/access/roles.md#指派.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"), ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20"), ("personId" = Option<Uuid>, Query, description = "Optional historical person UUID"), ("operationId" = Option<Uuid>, Query, description = "Optional original operation UUID; may match multiple families")),
    responses((status = 200, description = "Complete permanent appointment page", body = Appointments), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn assignments(
    Extension(actor): Extension<Actor>,
    State(service): State<appointment::Service>,
    Query(filter): Query<appointment::Filter>,
) -> Result<Response, Error> {
    let list = timeout(Duration::from_secs(30), service.list(&actor, filter)).await??;
    Ok(Json(Appointments {
        code: "ok",
        count: list.count(),
        assignments: list.appointments().iter().map(Appointment::from).collect(),
    })
    .into_response())
}

#[utoipa::path(post, path = "/assignments/candidates", operation_id = "assignmentCandidate",
    description = "Locate an eligible institutional appointment target by mobile and return its current confirmation. Contract: docs/access/roles.md#指派.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = Search, content_type = "application/json"),
    responses((status = 200, description = "Exact candidate", body = Candidate), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn candidate(
    Extension(actor): Extension<Actor>,
    State(service): State<appointment::Service>,
    Json(input): Json<Search>,
) -> Result<Response, Error> {
    let candidate = timeout(
        Duration::from_secs(30),
        service.candidate(&actor, input.mobile),
    )
    .await??;
    Ok(Json(Candidate {
        code: "ok",
        identity: Subject::from(candidate.identity()),
        assignment_id: candidate.assignment_id(),
        role: Role::from(candidate.role()),
    })
    .into_response())
}

#[utoipa::path(get, path = "/assignments/roles", operation_id = "assignableRoles",
    description = "List complete institutional roles the current manager can assign. Contract: docs/access/roles.md#指派.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"), ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20")),
    responses((status = 200, description = "Complete assignable role page", body = List), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn available(
    Extension(actor): Extension<Actor>,
    State(service): State<appointment::Service>,
    Query(pagination): Query<Pagination>,
) -> Result<Response, Error> {
    let list = timeout(Duration::from_secs(30), service.roles(&actor, pagination)).await??;
    Ok(Json(List {
        code: "ok",
        count: list.count(),
        roles: list.roles().iter().map(Role::from).collect(),
    })
    .into_response())
}

#[utoipa::path(post, path = "/assignments", operation_id = "assignRole",
    description = "Replace the confirmed institutional role and retain its permanent original appointment. Contract: docs/access/roles.md#指派.",
    params(("Operation-Id" = Uuid, Header, description = "Original website appointment UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = appointment::Input, content_type = "application/json"),
    responses((status = 201, description = "Complete original appointment", body = Assigned), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "request_mismatch, target_changed, role_changed or state_conflict", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn assign(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<appointment::Service>,
    Json(input): Json<appointment::Input>,
) -> Response {
    crate::route::command(service.assign(actor, operation_id, input), |assignment| {
        (
            StatusCode::CREATED,
            Json(Assigned {
                code: "ok",
                assignment: Appointment::from(&assignment),
            }),
        )
            .into_response()
    })
    .await
}

fn appointment_failure(failure: appointment::Failure) -> Response {
    let (status, code) = match failure {
        appointment::Failure::Invalid => (
            StatusCode::UNAUTHORIZED,
            Code::Common(Common::AuthenticationInvalid),
        ),
        appointment::Failure::Denied => (
            StatusCode::FORBIDDEN,
            Code::Common(Common::PermissionDenied),
        ),
        appointment::Failure::Mismatch => {
            (StatusCode::CONFLICT, Code::Common(Common::RequestMismatch))
        }
        appointment::Failure::Absent => (StatusCode::NOT_FOUND, Code::TargetUnavailable),
        appointment::Failure::TargetChanged => (StatusCode::CONFLICT, Code::TargetChanged),
        appointment::Failure::RoleChanged => (StatusCode::CONFLICT, Code::RoleChanged),
        appointment::Failure::Unchanged => (StatusCode::CONFLICT, Code::StateConflict),
        appointment::Failure::Busy => return crate::response::limited(Duration::from_secs(1)),
        appointment::Failure::Worker => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Code::Common(Common::InternalError),
        ),
        appointment::Failure::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Code::Common(Common::DependencyUnavailable),
        ),
        appointment::Failure::Unknown => (
            StatusCode::SERVICE_UNAVAILABLE,
            Code::Common(Common::OutcomeUnknown),
        ),
    };
    (status, Json(Failure { code })).into_response()
}

async fn target(Path(id): Path<role::Id>, mut request: Request, next: Next) -> Response {
    request.extensions_mut().insert(id);
    next.run(request).await
}

#[utoipa::path(put, path = "/roles/{id}", operation_id = "updateRole",
    description = "Replace a custom institutional role definition confirmed by changeId. Contract: docs/access/roles.md#角色办理.",
    params(("id" = Uuid, Path, description = "Institutional role UUID"),
        ("Operation-Id" = Uuid, Header, description = "Original institutional role operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID selecting its cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = role::Update, content_type = "application/json"),
    responses((status = 200, description = "Complete original role definition", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "request_mismatch, role_changed, role_name_taken or state_conflict", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure),
        (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure),
        (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn update(
    Extension(operation_id): Extension<operation::Id>,
    Extension(actor): Extension<Actor>,
    Extension(id): Extension<role::Id>,
    State(service): State<role::Service>,
    Json(input): Json<role::Update>,
) -> Response {
    route::command(service.update(actor, operation_id, id, input), |role| {
        Json(Receipt {
            code: "ok",
            role: Role::from(&role),
        })
        .into_response()
    })
    .await
}

#[utoipa::path(delete, path = "/roles/{id}", operation_id = "deleteRole",
    description = "Delete a custom institutional role confirmed by changeId. Contract: docs/access/roles.md#角色办理.",
    params(("id" = Uuid, Path, description = "Institutional role UUID"),
        ("Operation-Id" = Uuid, Header, description = "Original institutional role operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID selecting its cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = role::Delete, content_type = "application/json"),
    responses((status = 200, description = "Permanent original deletion", body = Deletion),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "request_mismatch, role_changed or role_in_use", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure),
        (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure),
        (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn delete(
    Extension(operation_id): Extension<operation::Id>,
    Extension(actor): Extension<Actor>,
    Extension(id): Extension<role::Id>,
    State(service): State<role::Service>,
    Json(input): Json<role::Delete>,
) -> Response {
    route::command(service.delete(actor, operation_id, id, input), |change| {
        Json(Deletion {
            code: "ok",
            change: Change::from(&change),
        })
        .into_response()
    })
    .await
}

#[utoipa::path(put, path = "/workforce/roles/{id}", operation_id = "updateWorkforceRole",
    description = "Replace a custom Workforce role definition through the selected authority entry. Contract: docs/access/roles.md#角色办理.",
    params(("id" = role::Id, Path, description = "Workforce role UUID"),
        ("Operation-Id" = Uuid, Header, description = "Original Workforce role operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID selecting its cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = role::workforce::Update, content_type = "application/json"),
    responses((status = 200, description = "Complete original role definition", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "request_mismatch, role_changed, role_name_taken or state_conflict", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure),
        (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure),
        (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn update_workforce(
    Extension(operation_id): Extension<operation::Id>,
    Extension(actor): Extension<Actor>,
    Extension(id): Extension<role::Id>,
    State(service): State<role::Service>,
    Json(input): Json<role::workforce::Update>,
) -> Response {
    route::command(
        service.update_workforce(actor, operation_id, id, input),
        |role| {
            Json(Receipt {
                code: "ok",
                role: Role::from(&role),
            })
            .into_response()
        },
    )
    .await
}

#[utoipa::path(delete, path = "/workforce/roles/{id}", operation_id = "deleteWorkforceRole",
    description = "Delete a custom Workforce role through the selected authority entry. Contract: docs/access/roles.md#角色办理.",
    params(("id" = role::Id, Path, description = "Workforce role UUID"),
        ("Operation-Id" = Uuid, Header, description = "Original Workforce role operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID selecting its cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = role::workforce::Delete, content_type = "application/json"),
    responses((status = 200, description = "Permanent original deletion", body = Deletion),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "request_mismatch, role_changed or role_in_use", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure),
        (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure),
        (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn delete_workforce(
    Extension(operation_id): Extension<operation::Id>,
    Extension(actor): Extension<Actor>,
    Extension(id): Extension<role::Id>,
    State(service): State<role::Service>,
    Json(input): Json<role::workforce::Delete>,
) -> Response {
    route::command(
        service.delete_workforce(actor, operation_id, id, input),
        |change| {
            Json(Deletion {
                code: "ok",
                change: Change::from(&change),
            })
            .into_response()
        },
    )
    .await
}

#[utoipa::path(post, path = "/roles", operation_id = "createRole",
    description = "Create a custom institutional role. Contract: docs/access/roles.md#角色办理.",
    params(("Operation-Id" = Uuid, Header, description = "Original institutional role operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID selecting its cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = role::Input, content_type = "application/json"),
    responses((status = 201, description = "Complete original role definition", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 409, description = "request_mismatch or role_name_taken", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure),
        (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure),
        (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn create(
    Extension(operation_id): Extension<operation::Id>,
    Extension(actor): Extension<Actor>,
    State(service): State<role::Service>,
    Json(input): Json<role::Input>,
) -> Response {
    route::command(service.create(actor, operation_id, input), |role| {
        (
            StatusCode::CREATED,
            Json(Receipt {
                code: "ok",
                role: Role::from(&role),
            }),
        )
            .into_response()
    })
    .await
}

#[utoipa::path(post, path = "/workforce/roles", operation_id = "createWorkforceRole",
    description = "Create a custom Workforce role through the selected authority entry. Contract: docs/access/roles.md#角色办理.",
    params(("Operation-Id" = Uuid, Header, description = "Original Workforce role operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID selecting its cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = role::workforce::Input, content_type = "application/json"),
    responses((status = 201, description = "Complete original role definition", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 409, description = "request_mismatch or role_name_taken", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure),
        (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure),
        (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn create_workforce(
    Extension(operation_id): Extension<operation::Id>,
    Extension(actor): Extension<Actor>,
    State(service): State<role::Service>,
    Json(input): Json<role::workforce::Input>,
) -> Response {
    route::command(
        service.create_workforce(actor, operation_id, input),
        |role| {
            (
                StatusCode::CREATED,
                Json(Receipt {
                    code: "ok",
                    role: Role::from(&role),
                }),
            )
                .into_response()
        },
    )
    .await
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

#[derive(Serialize, ToSchema)]
#[schema(as = role::Receipt)]
struct Receipt<'a> {
    code: &'static str,
    role: Role<'a>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = role::Deletion)]
struct Deletion<'a> {
    code: &'static str,
    change: Change<'a>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = role::Role)]
struct Role<'a> {
    id: role::Id,
    kind: role::Kind,
    name: &'a role::Name,
    level: role::Level,
    permissions: &'a [role::Code],
    change_id: role::change::Id,
}

impl<'a> From<&'a role::Role> for Role<'a> {
    fn from(role: &'a role::Role) -> Self {
        Self {
            id: role.id(),
            kind: role.kind(),
            name: role.name(),
            level: role.level(),
            permissions: role.permissions(),
            change_id: role.change_id(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = role::List)]
struct List<'a> {
    code: &'static str,
    #[schema(minimum = 0)]
    count: i64,
    roles: Vec<Role<'a>>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = role::change::Change)]
struct Change<'a> {
    id: role::change::Id,
    role_id: role::Id,
    #[serde(flatten)]
    action: &'a role::change::Action,
    name: &'a role::Name,
    level: role::Level,
    operator: Identity<'a>,
    changed_at: DateTime<Utc>,
    operation_id: operation::Id,
}

impl<'a> From<&'a role::change::Change> for Change<'a> {
    fn from(change: &'a role::change::Change) -> Self {
        Self {
            id: change.id(),
            role_id: change.role_id(),
            action: change.action(),
            name: change.name(),
            level: change.level(),
            operator: Identity::from(change.operator()),
            changed_at: change.changed_at(),
            operation_id: change.operation_id(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = role::change::List)]
struct Changes<'a> {
    code: &'static str,
    #[schema(minimum = 0)]
    count: i64,
    changes: Vec<Change<'a>>,
}

#[utoipa::path(get, path = "/roles/changes", operation_id = "listRoleChanges",
    description = "List permanent institutional role changes with masked original operators. Contract: docs/access/roles.md#变更历史.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20"),
        ("roleId" = Option<Uuid>, Query, description = "Optional original role UUID"),
        ("operationId" = Option<Uuid>, Query, description = "Optional original operation UUID")),
    responses((status = 200, description = "Complete filtered history page with masked original operators", body = Changes),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn changes(
    Extension(actor): Extension<Actor>,
    State(service): State<role::Service>,
    Query(filter): Query<role::change::Filter>,
) -> Response {
    route::query("access_read", service.changes(&actor, filter), |list| {
        Json(Changes {
            code: "ok",
            count: list.count(),
            changes: list.changes().iter().map(Change::from).collect(),
        })
        .into_response()
    })
    .await
}

#[utoipa::path(get, path = "/workforce/roles/changes", operation_id = "listWorkforceRoleChanges",
    description = "List permanent Workforce role changes with masked original operators. Contract: docs/access/roles.md#变更历史.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20"),
        ("roleId" = Option<Uuid>, Query, description = "Optional original role UUID"),
        ("operationId" = Option<Uuid>, Query, description = "Optional original operation UUID")),
    responses((status = 200, description = "Complete filtered history page with masked original operators", body = Changes),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn changes_workforce(
    Extension(actor): Extension<Actor>,
    State(service): State<role::Service>,
    Query(filter): Query<role::change::Filter>,
) -> Response {
    route::query(
        "access_read",
        service.changes_workforce(&actor, filter),
        |list| {
            Json(Changes {
                code: "ok",
                count: list.count(),
                changes: list.changes().iter().map(Change::from).collect(),
            })
            .into_response()
        },
    )
    .await
}

#[utoipa::path(get, path = "/accounts/current/role", operation_id = "currentRole",
    description = "Read the current website account's institutional role. Contract: docs/access/roles.md#本人角色与定义列表.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID; no persistent read operation"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID; selects its canonical cookie name; Authorization is rejected")),
    responses((status = 200, description = "Complete current institutional role", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds before another attempt"))),
        (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn read(
    Extension(actor): Extension<Actor>,
    State(service): State<role::Service>,
) -> Response {
    route::query("access_read", service.read_current(&actor), |role| {
        Json(Receipt {
            code: "ok",
            role: Role::from(&role),
        })
        .into_response()
    })
    .await
}

#[utoipa::path(get, path = "/roles", operation_id = "listRoles",
    description = "List complete current institutional role definitions. Contract: docs/access/roles.md#本人角色与定义列表.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID; repeating it reads current definitions"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID selecting its cookie"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1; (page - 1) * size must fit PostgreSQL bigint"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20")),
    responses((status = 200, description = "Authorized count and role page; out-of-range pages retain count", body = List),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds before another attempt"))),
        (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn list(
    Extension(actor): Extension<Actor>,
    State(service): State<role::Service>,
    Query(pagination): Query<Pagination>,
) -> Response {
    route::query("access_read", service.list(&actor, pagination), |list| {
        Json(List {
            code: "ok",
            count: list.count(),
            roles: list.roles().iter().map(Role::from).collect(),
        })
        .into_response()
    })
    .await
}

#[utoipa::path(get, path = "/workforce/roles", operation_id = "listWorkforceRoles",
    description = "List complete current Workforce role definitions. Contract: docs/access/roles.md#本人角色与定义列表.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID; repeating it reads current definitions"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID selecting its cookie"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1; (page - 1) * size must fit PostgreSQL bigint"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20")),
    responses((status = 200, description = "Authorized count and role page; out-of-range pages retain count", body = List),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds before another attempt"))),
        (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn list_workforce(
    Extension(actor): Extension<Actor>,
    State(service): State<role::Service>,
    Query(pagination): Query<Pagination>,
) -> Response {
    route::query(
        "access_read",
        service.list_workforce(&actor, pagination),
        |list| {
            Json(List {
                code: "ok",
                count: list.count(),
                roles: list.roles().iter().map(Role::from).collect(),
            })
            .into_response()
        },
    )
    .await
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("{0}")]
    Appointment(#[from] appointment::Error),
    #[error("access read budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::Appointment(cause) => {
                if cause.failure() == appointment::Failure::Unavailable {
                    tracing::error!(stage = "appointment_read", kind = "unavailable", error = ?cause);
                }
                return appointment_failure(cause.failure());
            }
            cause => {
                tracing::error!(stage = "access_read", kind = "unavailable", error = ?cause);
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Common::DependencyUnavailable,
                )
            }
        };
        (status, Json(Failure { code })).into_response()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use axum::{extract::Query, http::Uri};
    use domain::{
        access::{appointment, role::change::Filter},
        pagination::Pagination,
    };

    #[test]
    fn appointment_filters_use_the_actual_query_boundary() {
        let id = "00000000-0000-4000-8000-000000000001";
        for query in [
            String::new(),
            "page=1&size=100".to_owned(),
            format!("personId={id}"),
            format!("operationId={id}"),
            format!("personId={id}&size=1&operationId={id}&page=2"),
            format!("page=2&operationId={id}&size=1&personId={id}"),
        ] {
            assert!(
                Query::<appointment::Filter>::try_from_uri(
                    &format!("/assignments?{query}").parse::<Uri>().expect("URI")
                )
                .is_ok(),
                "{query}"
            );
        }
        for query in [
            "page=0",
            "size=0",
            "size=101",
            "page=-1",
            "page=1.5",
            "page=",
            "size=",
            "page=9223372036854775808",
            "size=9223372036854775808",
            "page=9223372036854775807&size=2",
            "page=1&page=2",
            "size=1&size=2",
            "personId=",
            "operationId=",
            "personId=no",
            "operationId=no",
            "personId=00000000-0000-4000-8000-000000000001&personId=00000000-0000-4000-8000-000000000001",
            "operationId=00000000-0000-4000-8000-000000000001&operationId=00000000-0000-4000-8000-000000000001",
            "person_id=00000000-0000-4000-8000-000000000001",
            "PersonId=00000000-0000-4000-8000-000000000001",
            "Page=1",
            "entry=institution",
            "roleId=00000000-0000-4000-8000-000000000001",
        ] {
            assert!(
                Query::<appointment::Filter>::try_from_uri(
                    &format!("/assignments?{query}").parse::<Uri>().expect("URI")
                )
                .is_err(),
                "{query}"
            );
        }
        for field in ["personId", "operationId"] {
            assert!(
                Query::<Pagination>::try_from_uri(
                    &format!("/assignments/roles?{field}={id}")
                        .parse::<Uri>()
                        .expect("URI")
                )
                .is_err()
            );
        }
    }

    #[test]
    fn history_queries_use_the_shared_strict_pagination_and_typed_optional_filters() {
        let id = "00000000-0000-4000-8000-000000000001";
        for query in [
            String::new(),
            "page=2&size=100".into(),
            "size=1&page=9223372036854775807".into(),
            format!("roleId={id}"),
            format!("operationId={id}"),
            format!("roleId={id}&page=2&operationId={id}&size=1"),
            format!("size=1&operationId={id}&page=2&roleId={id}"),
        ] {
            let uri: Uri = format!("/roles/changes?{query}").parse().expect("URI");
            assert!(Query::<Filter>::try_from_uri(&uri).is_ok(), "{query}");
        }
        for query in [
            "page=0",
            "page=-1",
            "size=0",
            "size=101",
            "page=1.5",
            "page=",
            "size=",
            "page=9223372036854775808",
            "size=9223372036854775808",
            "page=9223372036854775807&size=2",
            "page=1&page=2",
            "size=1&size=2",
            "roleId=",
            "operationId=",
            "roleId=no",
            "operationId=no",
            "roleId=00000000-0000-4000-8000-000000000001&roleId=00000000-0000-4000-8000-000000000001",
            "operationId=00000000-0000-4000-8000-000000000001&operationId=00000000-0000-4000-8000-000000000001",
            "role_id=00000000-0000-4000-8000-000000000001",
            "Page=1",
            "entry=institution",
        ] {
            let uri: Uri = format!("/roles/changes?{query}").parse().expect("URI");
            assert!(Query::<Filter>::try_from_uri(&uri).is_err(), "{query}");
        }
        for field in ["roleId", "operationId"] {
            let uri: Uri = format!("/roles?{field}={id}").parse().expect("URI");
            assert!(Query::<Pagination>::try_from_uri(&uri).is_err());
        }
    }
}
