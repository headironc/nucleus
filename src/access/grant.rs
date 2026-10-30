//! Workforce admission, authorized history, state management, and the holder's current role.

use std::time::Duration;

use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, FromRequest, Request, State, rejection::BytesRejection},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::{
    access::{
        Entry, appointment,
        grant::{self, invitation},
    },
    identity::{current::Actor, mobile::Mobile, operation, person, suspension},
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
    access::{Role, assignment::Assignment},
    extension::Extension,
    identity::{self, account::Subject, person::Identity},
    json::Json,
    path::Path,
    query::Query,
    response::{Common, Failure},
};

pub(super) fn router(service: grant::Service, authentication: identity::Context) -> OpenApiRouter {
    let mut router = OpenApiRouter::new()
        .routes(routes!(candidate))
        .routes(routes!(termination_candidate))
        .routes(routes!(suspension_candidate))
        .routes(routes!(resumption_candidate))
        .routes(routes!(create, list))
        .routes(routes!(list_suspensions))
        .routes(routes!(list_resumptions))
        .routes(routes!(list_terminations))
        .routes(routes!(suspend))
        .routes(routes!(resume))
        .routes(routes!(terminate))
        .routes(routes!(read))
        .routes(routes!(read_role))
        .routes(routes!(create_invitation))
        .routes(routes!(read_invitation, adjust_invitation))
        .routes(routes!(withdraw_invitation))
        .routes(routes!(invitation_candidate))
        .routes(routes!(accept_invitation))
        .route_layer(middleware::from_fn_with_state(
            authentication.clone(),
            identity::current::authenticate,
        ))
        .merge(
            OpenApiRouter::new()
                .routes(routes!(read_grant))
                .route_layer(middleware::from_fn_with_state(
                    authentication,
                    identity::current::authenticate,
                ))
                .route_layer(middleware::from_fn(grant_target)),
        )
        .layer(DefaultBodyLimit::max(8192))
        .with_state(service);
    // Absence selects self exit; JSON null is not a managed confirmation.
    if let Some(RefOr::T(body)) = router
        .get_openapi_mut()
        .paths
        .paths
        .get_mut("/workforce/grants/{id}/terminations")
        .and_then(|path| path.post.as_mut())
        .and_then(|operation| operation.request_body.as_mut())
    {
        body.required = Some(utoipa::openapi::Required::False);
    }
    router
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = grant::Search)]
struct Search {
    entry: Entry,
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: Mobile,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = grant::member::Search)]
struct MemberSearch {
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: Mobile,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = grant::member::Candidate)]
struct MemberCandidate<'a> {
    code: &'static str,
    identity: Subject<'a>,
    grant_id: grant::Id,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = grant::resumption::Candidate)]
struct ResumptionCandidate<'a> {
    code: &'static str,
    identity: Subject<'a>,
    suspension_id: grant::suspension::Id,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = grant::Candidate)]
struct Candidate<'a> {
    code: &'static str,
    identity: Subject<'a>,
    assignment_id: appointment::Id,
    #[schema(required)]
    suspension_id: Option<suspension::Id>,
    #[schema(required)]
    grant: Option<grant::Confirmation>,
    count: i64,
    roles: Vec<Role<'a>>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = grant::Grant)]
struct Grant<'a> {
    id: grant::Id,
    subject: Identity<'a>,
    operator: Identity<'a>,
    granted_at: DateTime<Utc>,
    sequence: i64,
    operation_id: operation::Id,
    status: grant::Status,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = grant::invitation::Invitation)]
struct Invitation<'a> {
    id: invitation::Id,
    role_id: domain::access::role::Id,
    capacity: invitation::Capacity,
    expires_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    operation_id: operation::Id,
    reasons: &'a [invitation::Reason],
    /// The original code while its creator may still copy a usable invitation.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, pattern = "^[A-Za-z0-9_-]{43}$")]
    code: Option<&'a invitation::Code>,
}

impl<'a> From<&'a invitation::Invitation> for Invitation<'a> {
    fn from(invitation: &'a invitation::Invitation) -> Self {
        Self {
            id: invitation.id(),
            role_id: invitation.role_id(),
            capacity: invitation.capacity(),
            expires_at: invitation.expires_at(),
            created_at: invitation.created_at(),
            operation_id: invitation.operation_id(),
            reasons: invitation.reasons(),
            code: invitation.code(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::invitation::Receipt)]
struct InvitationReceipt<'a> {
    code: &'static str,
    invitation: Invitation<'a>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::invitation::CandidateReceipt)]
struct InvitationCandidate {
    code: &'static str,
    candidate: invitation::acceptance::Candidate,
}

#[utoipa::path(post, path = "/workforce/invitations/candidates", operation_id = "checkWorkforceInvitation",
    description = "Read the invitation candidate available to the authenticated holder. Contract: docs/access/invitations.md#原结果与-http.",
    params(("Operation-Id" = Uuid, Header, description = "Request UUID"), ("Session-Id" = Uuid, Header, description = "Website session UUID"), ("Origin" = String, Header, description = "Required exact origin")),
    request_body(content = invitation::acceptance::Search, content_type = "application/json"),
    responses((status = 200, body = InvitationCandidate), (status = 400, body = Failure), (status = 401, body = Failure), (status = 403, body = Failure), (status = 409, description = "invitation_unavailable", body = Failure), (status = 413, body = Failure), (status = 415, body = Failure), (status = 422, body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, body = Failure), (status = 503, body = Failure)))]
async fn invitation_candidate(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<invitation::acceptance::Search>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.invitation_candidate(&actor, input),
    )
    .await
    {
        Ok(Ok(candidate)) => Json(InvitationCandidate {
            code: "ok",
            candidate,
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "invitation_candidate", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(_) => failed(grant::Failure::Unavailable),
    }
}

#[utoipa::path(post, path = "/workforce/acceptances", operation_id = "acceptWorkforceInvitation",
    description = "Accept the confirmed invitation and retain the original Grant and first assignment. Contract: docs/access/invitations.md#原结果与-http.",
    params(("Operation-Id" = Uuid, Header, description = "Original admission UUID"), ("Session-Id" = Uuid, Header, description = "Website session UUID"), ("Origin" = String, Header, description = "Required exact origin")),
    request_body(content = invitation::acceptance::Input, content_type = "application/json"),
    responses((status = 201, body = Receipt), (status = 400, body = Failure), (status = 401, body = Failure), (status = 403, body = Failure), (status = 409, description = "invitation_unavailable, role_changed, already_authorized or request_mismatch", body = Failure), (status = 413, body = Failure), (status = 415, body = Failure), (status = 422, body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn accept_invitation(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<grant::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<invitation::acceptance::Input>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.accept_invitation(actor, operation_id, input),
    )
    .await
    {
        Ok(Ok((grant, assignment))) => (
            StatusCode::CREATED,
            Json(Receipt {
                code: "ok",
                grant: Grant::from(&grant),
                assignment: Assignment::from(&assignment),
            }),
        )
            .into_response(),
        Ok(Err(error)) => failed(error.failure()),
        Err(_) => failed(grant::Failure::Unknown),
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::List)]
struct List<'a> {
    code: &'static str,
    count: i64,
    grants: Vec<Grant<'a>>,
}

#[utoipa::path(get, path = "/workforce/grants", operation_id = "listWorkforceGrants",
    description = "List permanent Workforce admissions with fixed masked identities and current status. Contract: docs/access/grants.md#历史读取.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20"),
        ("personId" = Option<person::Id>, Query, description = "Optional permanent person UUID"),
        ("grantId" = Option<grant::Id>, Query, description = "Optional permanent Grant UUID"),
        ("operationId" = Option<operation::Id>, Query, description = "Optional original admission operation UUID")),
    responses((status = 200, description = "Complete permanent Grant page", body = List),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 413, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn list(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
    Query(filter): Query<grant::Filter>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = validate_empty(body) {
        return (
            status,
            Json(Failure {
                code: Common::InvalidInput,
            }),
        )
            .into_response();
    }
    match timeout(Duration::from_secs(30), service.list(&actor, filter)).await {
        Ok(Ok(list)) => Json(List {
            code: "ok",
            count: list.count(),
            grants: list.grants().iter().map(Grant::from).collect(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "grant_history", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "grant_history", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::suspension::List)]
struct SuspensionList<'a> {
    code: &'static str,
    count: i64,
    suspensions: Vec<Suspension<'a>>,
}

#[utoipa::path(get, path = "/workforce/suspensions", operation_id = "listWorkforceSuspensions",
    description = "List permanent Workforce suspensions with fixed masked identities. Contract: docs/access/grants.md#历史读取.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20"),
        ("personId" = Option<person::Id>, Query, description = "Optional permanent person UUID"),
        ("grantId" = Option<grant::Id>, Query, description = "Optional permanent Grant UUID"),
        ("operationId" = Option<operation::Id>, Query, description = "Optional original suspension operation UUID")),
    responses((status = 200, description = "Complete permanent suspension page", body = SuspensionList),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 413, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn list_suspensions(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
    Query(filter): Query<grant::Filter>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = validate_empty(body) {
        return (
            status,
            Json(Failure {
                code: Common::InvalidInput,
            }),
        )
            .into_response();
    }
    match timeout(
        Duration::from_secs(30),
        service.list_suspensions(&actor, filter),
    )
    .await
    {
        Ok(Ok(list)) => Json(SuspensionList {
            code: "ok",
            count: list.count(),
            suspensions: list.suspensions().iter().map(Suspension::from).collect(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "grant_suspension_history", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "grant_suspension_history", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::resumption::List)]
struct ResumptionList<'a> {
    code: &'static str,
    count: i64,
    resumptions: Vec<Resumption<'a>>,
}

#[utoipa::path(get, path = "/workforce/resumptions", operation_id = "listWorkforceResumptions",
    description = "List permanent Workforce resumptions with fixed masked identities. Contract: docs/access/grants.md#历史读取.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20"),
        ("personId" = Option<person::Id>, Query, description = "Optional permanent person UUID"),
        ("grantId" = Option<grant::Id>, Query, description = "Optional permanent Grant UUID"),
        ("operationId" = Option<operation::Id>, Query, description = "Optional original resumption operation UUID")),
    responses((status = 200, description = "Complete permanent resumption page", body = ResumptionList),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 413, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn list_resumptions(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
    Query(filter): Query<grant::Filter>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = validate_empty(body) {
        return (
            status,
            Json(Failure {
                code: Common::InvalidInput,
            }),
        )
            .into_response();
    }
    match timeout(
        Duration::from_secs(30),
        service.list_resumptions(&actor, filter),
    )
    .await
    {
        Ok(Ok(list)) => Json(ResumptionList {
            code: "ok",
            count: list.count(),
            resumptions: list.resumptions().iter().map(Resumption::from).collect(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "grant_resumption_history", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "grant_resumption_history", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::termination::List)]
struct TerminationList<'a> {
    code: &'static str,
    count: i64,
    terminations: Vec<Termination<'a>>,
}

#[utoipa::path(get, path = "/workforce/terminations", operation_id = "listWorkforceTerminations",
    description = "List self, manager and cancellation endings in time and ID order. Contract: docs/access/grants.md#历史读取.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20"),
        ("personId" = Option<person::Id>, Query, description = "Optional permanent person UUID"),
        ("grantId" = Option<grant::Id>, Query, description = "Optional ended Grant UUID"),
        ("operationId" = Option<operation::Id>, Query, description = "Optional original termination or cancellation operation UUID")),
    responses((status = 200, description = "Complete permanent termination page", body = TerminationList),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 413, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn list_terminations(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
    Query(filter): Query<grant::Filter>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = validate_empty(body) {
        return (
            status,
            Json(Failure {
                code: Common::InvalidInput,
            }),
        )
            .into_response();
    }
    match timeout(
        Duration::from_secs(30),
        service.list_terminations(&actor, filter),
    )
    .await
    {
        Ok(Ok(list)) => Json(TerminationList {
            code: "ok",
            count: list.count(),
            terminations: list.terminations().iter().map(Termination::from).collect(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "grant_termination_history", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "grant_termination_history", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

impl<'a> From<&'a grant::Grant> for Grant<'a> {
    fn from(grant: &'a grant::Grant) -> Self {
        Self {
            id: grant.id(),
            subject: Identity::from(grant.subject()),
            operator: Identity::from(grant.operator()),
            granted_at: grant.granted_at(),
            sequence: grant.sequence(),
            operation_id: grant.operation_id(),
            status: grant.status(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::Receipt)]
struct Receipt<'a> {
    code: &'static str,
    grant: Grant<'a>,
    assignment: Assignment<'a>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = termination::Termination)]
struct Termination<'a> {
    id: grant::Id,
    subject: Identity<'a>,
    operator: Identity<'a>,
    mode: grant::termination::Mode,
    terminated_at: DateTime<Utc>,
    sequence: i64,
    operation_id: operation::Id,
}

impl<'a> From<&'a grant::termination::Termination> for Termination<'a> {
    fn from(termination: &'a grant::termination::Termination) -> Self {
        Self {
            id: termination.id(),
            subject: Identity::from(termination.subject()),
            operator: Identity::from(termination.operator()),
            mode: termination.mode(),
            terminated_at: termination.terminated_at(),
            sequence: termination.sequence(),
            operation_id: termination.operation_id(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = termination::Receipt)]
struct TerminationReceipt<'a> {
    code: &'static str,
    termination: Termination<'a>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = grant::suspension::Suspension)]
struct Suspension<'a> {
    id: grant::suspension::Id,
    grant_id: grant::Id,
    subject: Identity<'a>,
    operator: Identity<'a>,
    suspended_at: DateTime<Utc>,
    sequence: i64,
    operation_id: operation::Id,
}

impl<'a> From<&'a grant::suspension::Suspension> for Suspension<'a> {
    fn from(suspension: &'a grant::suspension::Suspension) -> Self {
        Self {
            id: suspension.id(),
            grant_id: suspension.grant_id(),
            subject: Identity::from(suspension.subject()),
            operator: Identity::from(suspension.operator()),
            suspended_at: suspension.suspended_at(),
            sequence: suspension.sequence(),
            operation_id: suspension.operation_id(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::suspension::Receipt)]
struct SuspensionReceipt<'a> {
    code: &'static str,
    suspension: Suspension<'a>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = grant::resumption::Resumption)]
struct Resumption<'a> {
    id: grant::suspension::Id,
    grant_id: grant::Id,
    subject: Identity<'a>,
    operator: Identity<'a>,
    resumed_at: DateTime<Utc>,
    sequence: i64,
    operation_id: operation::Id,
}

impl<'a> From<&'a grant::resumption::Resumption> for Resumption<'a> {
    fn from(resumption: &'a grant::resumption::Resumption) -> Self {
        Self {
            id: resumption.id(),
            grant_id: resumption.grant_id(),
            subject: Identity::from(resumption.subject()),
            operator: Identity::from(resumption.operator()),
            resumed_at: resumption.resumed_at(),
            sequence: resumption.sequence(),
            operation_id: resumption.operation_id(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::resumption::Receipt)]
struct ResumptionReceipt<'a> {
    code: &'static str,
    resumption: Resumption<'a>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::Current)]
struct Current<'a> {
    code: &'static str,
    grant: Grant<'a>,
}

#[utoipa::path(get, path = "/workforce/current/grant", operation_id = "currentWorkforceGrant",
    description = "Read the current person's latest Workforce Grant and status. Contract: docs/access/grants.md#http-入口.",
    params(("Operation-Id" = Uuid, Header, description = "This read request UUID"), ("Session-Id" = Uuid, Header, description = "Selects a currently valid website session cookie")),
    responses((status = 200, description = "Latest grant with fixed masked identities and current status", body = Current), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "grant_absent", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn read(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = validate_empty(body) {
        return (
            status,
            Json(Failure {
                code: Common::InvalidInput,
            }),
        )
            .into_response();
    }
    match timeout(Duration::from_secs(30), service.read_latest(&actor)).await {
        Ok(Ok(grant)) => Json(Current {
            code: "ok",
            grant: Grant::from(&grant),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "grant_read", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "grant_read", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

#[utoipa::path(get, path = "/workforce/grants/{id}", operation_id = "readWorkforceGrant",
    description = "Read an authorized permanent Grant and its fixed first assignment. Contract: docs/access/grants.md#http-入口.",
    params(("id" = grant::Id, Path, description = "Permanent Grant UUID"), ("Operation-Id" = Uuid, Header, description = "This read request UUID"), ("Session-Id" = Uuid, Header, description = "Selects a currently valid website session cookie")),
    responses((status = 200, description = "Grant and original first Assignment", body = Receipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn read_grant(
    Extension(actor): Extension<Actor>,
    Extension(id): Extension<grant::Id>,
    State(service): State<grant::Service>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = validate_empty(body) {
        return (
            status,
            Json(Failure {
                code: Common::InvalidInput,
            }),
        )
            .into_response();
    }
    match timeout(Duration::from_secs(30), service.read(&actor, id)).await {
        Ok(Ok((grant, assignment))) => Json(Receipt {
            code: "ok",
            grant: Grant::from(&grant),
            assignment: Assignment::from(&assignment),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "grant_management_read", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "grant_management_read", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

async fn grant_target(Path(id): Path<grant::Id>, mut request: Request, next: Next) -> Response {
    request.extensions_mut().insert(id);
    next.run(request).await
}

fn validate_empty(body: Result<Bytes, BytesRejection>) -> Result<(), StatusCode> {
    match body {
        Ok(body) if body.is_empty() => Ok(()),
        Err(error) => Err(error.status()),
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

#[utoipa::path(get, path = "/workforce/current/role", operation_id = "currentWorkforceRole",
    description = "Read the current person's complete Workforce role. Contract: docs/access/roles.md#本人角色与定义列表.",
    params(("Operation-Id" = Uuid, Header, description = "This read request UUID"), ("Session-Id" = Uuid, Header, description = "Selects a currently valid website session cookie")),
    responses((status = 200, description = "Complete current Workforce role", body = crate::access::Receipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "role_absent", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn read_role(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = validate_empty(body) {
        return (
            status,
            Json(Failure {
                code: Common::InvalidInput,
            }),
        )
            .into_response();
    }
    match timeout(Duration::from_secs(30), service.read_role(&actor)).await {
        Ok(Ok(role)) => Json(crate::access::Receipt {
            code: "ok",
            role: Role::from(&role),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "workforce_role_read", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "workforce_role_read", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

#[utoipa::path(post, path = "/workforce/grants/candidates", operation_id = "workforceGrantCandidate",
    description = "Confirm a Workforce admission target and eligible roles through the selected authority. Contract: docs/access/grants.md#http-入口.",
    params(("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1; (page - 1) * size must fit PostgreSQL bigint"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20"),
        ("Operation-Id" = Uuid, Header, description = "Required request UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = Search, content_type = "application/json"),
    responses((status = 200, description = "Masked candidate and role choices", body = Candidate), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn candidate(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
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
            assignment_id: candidate.assignment_id(),
            suspension_id: candidate.suspension_id(),
            grant: candidate.grant(),
            count: candidate.count(),
            roles: candidate.roles().iter().map(Role::from).collect(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "grant_candidate", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "grant_candidate", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

#[utoipa::path(post, path = "/workforce/grants", operation_id = "createWorkforceGrant",
    description = "Admit the confirmed target to Workforce and retain the original Grant and first assignment. Contract: docs/access/grants.md#http-入口.",
    params(("Operation-Id" = Uuid, Header, description = "Original admission UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = grant::Input, content_type = "application/json"),
    responses((status = 201, description = "Original grant and first assignment", body = Receipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "request_mismatch, target_changed, role_changed or already_authorized", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input or account_not_ready", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn create(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<grant::Service>,
    Json(input): Json<grant::Input>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.create(actor, operation_id, input),
    )
    .await
    {
        Ok(Ok((grant, assignment))) => (
            StatusCode::CREATED,
            Json(Receipt {
                code: "ok",
                grant: Grant::from(&grant),
                assignment: Assignment::from(&assignment),
            }),
        )
            .into_response(),
        Ok(Err(error)) => failed(error.failure()),
        Err(_) => failed(grant::Failure::Unknown),
    }
}

#[utoipa::path(post, path = "/workforce/terminations/candidates", operation_id = "workforceTerminationCandidate",
    description = "Confirm a manageable Workforce member and current Grant by mobile. Contract: docs/access/grants.md#http-入口.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = MemberSearch, content_type = "application/json"),
    responses((status = 200, description = "Masked target and exact current Grant", body = MemberCandidate), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn termination_candidate(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<MemberSearch>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.termination_candidate(&actor, input.mobile),
    )
    .await
    {
        Ok(Ok(candidate)) => Json(MemberCandidate {
            code: "ok",
            identity: Subject::from(candidate.identity()),
            grant_id: candidate.grant_id(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "termination_candidate", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "termination_candidate", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

#[utoipa::path(post, path = "/workforce/grants/{id}/terminations", operation_id = "terminateWorkforceGrant",
    description = "End the exact Grant through self exit or confirmed management action. Contract: docs/access/grants.md#人工结束与注销.",
    params(("id" = grant::Id, Path, description = "The exact original Workforce Grant"), ("Operation-Id" = Uuid, Header, description = "Original termination UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = grant::termination::Input, content_type = "application/json", description = "Omit the body for self exit; a nonempty body must contain the complete managed target confirmation."),
    responses((status = 201, description = "Original termination with fixed masked identities and self or manager mode", body = TerminationReceipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "request_mismatch, target_changed or state_conflict", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn terminate(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<grant::Service>,
    Path(id): Path<grant::Id>,
    Query(_): Query<Empty>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(error) => {
            return (
                error.status(),
                Json(Failure {
                    code: Common::InvalidInput,
                }),
            )
                .into_response();
        }
    };
    let result = if body.is_empty() {
        timeout(
            Duration::from_secs(30),
            service.terminate(actor, operation_id, id),
        )
        .await
    } else {
        let mut request = Request::new(Body::from(body));
        *request.headers_mut() = headers;
        let Json(input) = match Json::<grant::termination::Input>::from_request(request, &()).await
        {
            Ok(input) => input,
            Err(error) => return error.into_response(),
        };
        timeout(
            Duration::from_secs(30),
            service.terminate_member(actor, operation_id, id, input),
        )
        .await
    };
    match result {
        Ok(Ok(termination)) => (
            StatusCode::CREATED,
            Json(TerminationReceipt {
                code: "ok",
                termination: Termination::from(&termination),
            }),
        )
            .into_response(),
        Ok(Err(error)) => failed(error.failure()),
        Err(_) => failed(grant::Failure::Unknown),
    }
}

#[utoipa::path(post, path = "/workforce/suspensions/candidates", operation_id = "workforceSuspensionCandidate",
    description = "Confirm a manageable Workforce member and current Grant for suspension. Contract: docs/access/grants.md#http-入口.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = MemberSearch, content_type = "application/json"),
    responses((status = 200, description = "Masked target and exact current Grant", body = MemberCandidate), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn suspension_candidate(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<MemberSearch>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.suspension_candidate(&actor, input.mobile),
    )
    .await
    {
        Ok(Ok(candidate)) => Json(MemberCandidate {
            code: "ok",
            identity: Subject::from(candidate.identity()),
            grant_id: candidate.grant_id(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "suspension_candidate", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "suspension_candidate", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

#[utoipa::path(post, path = "/workforce/grants/{id}/suspensions", operation_id = "suspendWorkforceGrant",
    description = "Suspend the confirmed active Grant and retain the original result. Contract: docs/access/grants.md#暂停与恢复.",
    params(("id" = grant::Id, Path, description = "The exact original Workforce Grant"), ("Operation-Id" = Uuid, Header, description = "Original suspension UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = grant::suspension::Input, content_type = "application/json"),
    responses((status = 201, description = "Original suspension with fixed masked identities", body = SuspensionReceipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "request_mismatch, target_changed or state_conflict", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn suspend(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<grant::Service>,
    Path(id): Path<grant::Id>,
    Query(_): Query<Empty>,
    Json(input): Json<grant::suspension::Input>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.suspend(actor, operation_id, id, input),
    )
    .await
    {
        Ok(Ok(suspension)) => (
            StatusCode::CREATED,
            Json(SuspensionReceipt {
                code: "ok",
                suspension: Suspension::from(&suspension),
            }),
        )
            .into_response(),
        Ok(Err(error)) => failed(error.failure()),
        Err(_) => failed(grant::Failure::Unknown),
    }
}

#[utoipa::path(post, path = "/workforce/resumptions/candidates", operation_id = "workforceResumptionCandidate",
    description = "Confirm a manageable Workforce member and exact open suspension. Contract: docs/access/grants.md#http-入口.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = MemberSearch, content_type = "application/json"),
    responses((status = 200, description = "Masked identity and exact open suspension", body = ResumptionCandidate), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "state_conflict", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn resumption_candidate(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<MemberSearch>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.resumption_candidate(&actor, input.mobile),
    )
    .await
    {
        Ok(Ok(candidate)) => Json(ResumptionCandidate {
            code: "ok",
            identity: Subject::from(candidate.identity()),
            suspension_id: candidate.suspension_id(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "resumption_candidate", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "resumption_candidate", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

#[utoipa::path(post, path = "/workforce/suspensions/{id}/resumptions", operation_id = "resumeWorkforceGrant",
    description = "Resume the confirmed open Grant suspension and retain the original result. Contract: docs/access/grants.md#暂停与恢复.",
    params(("id" = grant::suspension::Id, Path, description = "The exact original Workforce suspension"), ("Operation-Id" = Uuid, Header, description = "Original resumption UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = grant::resumption::Input, content_type = "application/json"),
    responses((status = 201, description = "Original resumption with fixed masked identities", body = ResumptionReceipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "request_mismatch, target_changed or state_conflict", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn resume(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<grant::Service>,
    Path(id): Path<grant::suspension::Id>,
    Query(_): Query<Empty>,
    Json(input): Json<grant::resumption::Input>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.resume(actor, operation_id, id, input),
    )
    .await
    {
        Ok(Ok(resumption)) => (
            StatusCode::CREATED,
            Json(ResumptionReceipt {
                code: "ok",
                resumption: Resumption::from(&resumption),
            }),
        )
            .into_response(),
        Ok(Err(error)) => failed(error.failure()),
        Err(_) => failed(grant::Failure::Unknown),
    }
}

#[utoipa::path(post, path = "/workforce/invitations", operation_id = "createWorkforceInvitation",
    description = "Create an invitation and return its currently copyable original code. Contract: docs/access/invitations.md#原结果与-http.",
    params(("Operation-Id" = Uuid, Header, description = "Original creation UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = invitation::Input, content_type = "application/json"),
    responses((status = 201, description = "Original invitation", body = InvitationReceipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 409, description = "request_mismatch or role_changed", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input or expiry_passed", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn create_invitation(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<grant::Service>,
    Json(input): Json<invitation::Input>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.create_invitation(actor, operation_id, input),
    )
    .await
    {
        Ok(Ok(invitation)) => (
            StatusCode::CREATED,
            Json(InvitationReceipt {
                code: "ok",
                invitation: Invitation::from(&invitation),
            }),
        )
            .into_response(),
        Ok(Err(error)) => failed(error.failure()),
        Err(_) => failed(grant::Failure::Unknown),
    }
}

#[utoipa::path(get, path = "/workforce/invitations/{id}", operation_id = "readWorkforceInvitation",
    description = "Read the creator's invitation, current reasons and permitted original code. Contract: docs/access/invitations.md#原结果与-http.",
    params(("id" = invitation::Id, Path, description = "Invitation ID"), ("Operation-Id" = Uuid, Header, description = "This read request UUID"), ("Session-Id" = Uuid, Header, description = "Selects a currently valid website session cookie")),
    responses((status = 200, description = "Invitation and its current reasons", body = InvitationReceipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn read_invitation(
    Extension(actor): Extension<Actor>,
    State(service): State<grant::Service>,
    Path(id): Path<invitation::Id>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = validate_empty(body) {
        return (
            status,
            Json(Failure {
                code: Common::InvalidInput,
            }),
        )
            .into_response();
    }
    match timeout(Duration::from_secs(30), service.read_invitation(&actor, id)).await {
        Ok(Ok(invitation)) => Json(InvitationReceipt {
            code: "ok",
            invitation: Invitation::from(&invitation),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == grant::Failure::Unavailable {
                tracing::error!(stage = "invitation_read", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "invitation_read", kind = "timeout", error = ?error);
            failed(grant::Failure::Unavailable)
        }
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = grant::invitation::withdrawal::Withdrawal)]
struct Withdrawal {
    id: invitation::Id,
    withdrawn_at: DateTime<Utc>,
    operation_id: operation::Id,
}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::invitation::withdrawal::Receipt)]
struct WithdrawalReceipt {
    code: &'static str,
    withdrawal: Withdrawal,
}

#[utoipa::path(post, path = "/workforce/invitations/{id}/withdrawals", operation_id = "withdrawWorkforceInvitation",
    description = "Permanently withdraw the creator's invitation and retain its original result. Contract: docs/access/invitations.md#原结果与-http.",
    params(("id" = invitation::Id, Path, description = "Invitation ID"), ("Operation-Id" = Uuid, Header, description = "Original withdrawal UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    responses((status = 201, description = "Original withdrawal", body = WithdrawalReceipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "request_mismatch or state_conflict", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn withdraw_invitation(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<grant::Service>,
    Path(id): Path<invitation::Id>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = validate_empty(body) {
        return (
            status,
            Json(Failure {
                code: Common::InvalidInput,
            }),
        )
            .into_response();
    }
    match timeout(
        Duration::from_secs(30),
        service.withdraw_invitation(actor, operation_id, id),
    )
    .await
    {
        Ok(Ok(withdrawal)) => (
            StatusCode::CREATED,
            Json(WithdrawalReceipt {
                code: "ok",
                withdrawal: Withdrawal {
                    id: withdrawal.id(),
                    withdrawn_at: withdrawal.withdrawn_at(),
                    operation_id: withdrawal.operation_id(),
                },
            }),
        )
            .into_response(),
        Ok(Err(error)) => failed(error.failure()),
        Err(_) => failed(grant::Failure::Unknown),
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = grant::invitation::adjustment::Adjustment)]
struct Adjustment {
    id: invitation::adjustment::Id,
    invitation_id: invitation::Id,
    capacity: invitation::Capacity,
    expires_at: invitation::Expiry,
    adjusted_at: DateTime<Utc>,
    operation_id: operation::Id,
}

#[derive(Serialize, ToSchema)]
#[schema(as = grant::invitation::adjustment::Receipt)]
struct AdjustmentReceipt {
    code: &'static str,
    adjustment: Adjustment,
}

#[utoipa::path(put, path = "/workforce/invitations/{id}", operation_id = "adjustWorkforceInvitation",
    description = "Adjust the creator's invitation capacity and expiry against confirmed current terms. Contract: docs/access/invitations.md#原结果与-http.",
    params(("id" = invitation::Id, Path, description = "Invitation ID"), ("Operation-Id" = Uuid, Header, description = "Original adjustment UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body = invitation::adjustment::Input,
    responses((status = 200, body = AdjustmentReceipt), (status = 400, body = Failure), (status = 401, body = Failure), (status = 403, body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "request_mismatch, target_changed or state_conflict", body = Failure), (status = 413, body = Failure), (status = 415, body = Failure), (status = 422, description = "invalid_input, capacity_below_usage or expiry_passed", body = Failure), (status = 429, body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn adjust_invitation(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<grant::Service>,
    Path(id): Path<invitation::Id>,
    Query(_): Query<Empty>,
    Json(input): Json<invitation::adjustment::Input>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        service.adjust_invitation(actor, operation_id, id, input),
    )
    .await
    {
        Ok(Ok(adjustment)) => Json(AdjustmentReceipt {
            code: "ok",
            adjustment: Adjustment {
                id: adjustment.id(),
                invitation_id: adjustment.invitation_id(),
                capacity: adjustment.capacity(),
                expires_at: adjustment.expires_at(),
                adjusted_at: adjustment.adjusted_at(),
                operation_id: adjustment.operation_id(),
            },
        })
        .into_response(),
        Ok(Err(error)) => failed(error.failure()),
        Err(_) => failed(grant::Failure::Unknown),
    }
}

#[derive(Clone, Copy, IntoStaticStr, Serialize)]
#[strum(serialize_all = "snake_case")]
#[serde(into = "&'static str")]
enum Code {
    #[strum(transparent)]
    Common(Common),
    TargetUnavailable,
    GrantAbsent,
    RoleAbsent,
    TargetChanged,
    RoleChanged,
    AccountNotReady,
    AlreadyAuthorized,
    StateConflict,
    ExpiryPassed,
    CapacityBelowUsage,
    InvitationUnavailable,
}

impl crate::response::AsCode for Code {}
impl PartialSchema for Code {
    fn schema() -> RefOr<Schema> {
        Common::schema()
    }
}
impl ToSchema for Code {}

fn failed(failure: grant::Failure) -> Response {
    let (status, code) = match failure {
        grant::Failure::Invalid => (
            StatusCode::UNAUTHORIZED,
            Code::Common(Common::AuthenticationInvalid),
        ),
        grant::Failure::Denied => (
            StatusCode::FORBIDDEN,
            Code::Common(Common::PermissionDenied),
        ),
        grant::Failure::Mismatch => (StatusCode::CONFLICT, Code::Common(Common::RequestMismatch)),
        grant::Failure::Absent => (StatusCode::NOT_FOUND, Code::TargetUnavailable),
        grant::Failure::Missing => (StatusCode::NOT_FOUND, Code::GrantAbsent),
        grant::Failure::RoleAbsent => (StatusCode::NOT_FOUND, Code::RoleAbsent),
        grant::Failure::TargetChanged => (StatusCode::CONFLICT, Code::TargetChanged),
        grant::Failure::RoleChanged => (StatusCode::CONFLICT, Code::RoleChanged),
        grant::Failure::AccountNotReady => {
            (StatusCode::UNPROCESSABLE_ENTITY, Code::AccountNotReady)
        }
        grant::Failure::AlreadyAuthorized => (StatusCode::CONFLICT, Code::AlreadyAuthorized),
        grant::Failure::StateConflict => (StatusCode::CONFLICT, Code::StateConflict),
        grant::Failure::Expiry => (StatusCode::UNPROCESSABLE_ENTITY, Code::ExpiryPassed),
        grant::Failure::Capacity => (StatusCode::UNPROCESSABLE_ENTITY, Code::CapacityBelowUsage),
        grant::Failure::Unchanged => (StatusCode::CONFLICT, Code::StateConflict),
        grant::Failure::InvitationUnavailable => {
            (StatusCode::CONFLICT, Code::InvitationUnavailable)
        }
        grant::Failure::Busy => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "1")],
                Json(Failure {
                    code: Common::RateLimited,
                }),
            )
                .into_response();
        }
        grant::Failure::Worker => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Code::Common(Common::InternalError),
        ),
        grant::Failure::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Code::Common(Common::DependencyUnavailable),
        ),
        grant::Failure::Unknown => (
            StatusCode::SERVICE_UNAVAILABLE,
            Code::Common(Common::OutcomeUnknown),
        ),
    };
    (status, Json(Failure { code })).into_response()
}
