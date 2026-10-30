//! Self-service and qualified management of person-owned affiliations.
//! HTTP contracts and permissions: `docs/center/affiliation.md#http-契约`.

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Request, State, rejection::BytesRejection},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::{
    center::affiliation,
    identity::{account, current::Actor, mobile::Mobile, operation, person, person::revision},
    pagination::Pagination,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    extension::Extension,
    identity,
    json::Json,
    path::Path,
    query::Query,
    response::{Common, Failure},
    route,
};

pub(crate) fn router(
    service: affiliation::Service,
    authentication: identity::Context,
) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(values))
        .routes(routes!(create))
        .routes(routes!(read))
        .routes(routes!(history))
        .routes(routes!(candidate_by_mobile))
        .route_layer(middleware::from_fn_with_state(
            authentication.clone(),
            identity::current::authenticate,
        ))
        .merge(
            OpenApiRouter::new()
                .routes(routes!(candidate_by_person))
                .routes(routes!(read_person, correct))
                .routes(routes!(history_by_person))
                .route_layer(middleware::from_fn_with_state(
                    authentication,
                    identity::current::authenticate,
                ))
                .route_layer(middleware::from_fn(target)),
        )
        .layer(DefaultBodyLimit::max(8192))
        .with_state(service)
}

async fn target(Path(id): Path<person::Id>, mut request: Request, next: Next) -> Response {
    request.extensions_mut().insert(id);
    next.run(request).await
}

#[derive(Serialize, ToSchema)]
#[schema(as = affiliation::Receipt)]
struct Receipt {
    code: &'static str,
}

#[derive(Serialize, ToSchema)]
#[schema(as = affiliation::Current)]
struct Current {
    code: &'static str,
    affiliation: affiliation::Affiliation,
}

#[derive(Serialize, ToSchema)]
#[schema(as = affiliation::Values)]
struct Values {
    code: &'static str,
    affiliations: Vec<NamedCode>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = affiliation::NamedCode)]
struct NamedCode {
    value: affiliation::Code,
    name: &'static str,
}

#[derive(Serialize, ToSchema)]
#[schema(as = affiliation::History)]
struct History<'a> {
    code: &'static str,
    count: i64,
    affiliations: &'a [affiliation::Affiliation],
}

#[derive(Serialize, ToSchema)]
#[schema(as = affiliation::management::History)]
struct ManagementHistory<'a> {
    code: &'static str,
    count: i64,
    affiliations: Vec<ManagementAffiliation<'a>>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = affiliation::management::Affiliation)]
struct ManagementAffiliation<'a> {
    value: affiliation::Code,
    created_at: DateTime<Utc>,
    operator: identity::person::Identity<'a>,
}

impl<'a> From<&'a affiliation::management::Affiliation> for ManagementAffiliation<'a> {
    fn from(affiliation: &'a affiliation::management::Affiliation) -> Self {
        Self {
            value: affiliation.value(),
            created_at: affiliation.created_at(),
            operator: affiliation.operator().into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn validate_empty(body: Result<Bytes, BytesRejection>) -> Result<(), (StatusCode, Json<Failure>)> {
    let status = match body {
        Ok(body) if body.is_empty() => return Ok(()),
        Err(error) => error.status(),
        _ => StatusCode::BAD_REQUEST,
    };
    Err((
        status,
        Json(Failure {
            code: Common::InvalidInput,
        }),
    ))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = affiliation::Search)]
struct Search {
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: Mobile,
}

#[derive(Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[schema(as = affiliation::Target)]
enum Target {
    #[serde(rename_all = "camelCase")]
    Account {
        account_id: account::Id,
        person_id: person::Id,
        revision_id: revision::Id,
    },
    #[serde(rename_all = "camelCase")]
    Person {
        person_id: person::Id,
        revision_id: revision::Id,
    },
}

#[derive(Serialize, ToSchema)]
#[schema(as = affiliation::Candidate)]
struct Candidate<'a> {
    target: Target,
    name: &'a str,
}

#[derive(Serialize, ToSchema)]
#[schema(as = affiliation::CandidateReceipt)]
struct CandidateReceipt<'a> {
    code: &'static str,
    candidate: Candidate<'a>,
}

#[utoipa::path(post, path = "/affiliations/candidates", operation_id = "affiliationCandidateByMobile",
    description = "Locate an account by exact mobile with institution.affiliations.read or institution.affiliations.correct. Returns only masked identity and target confirmation. Requires exact Origin; supplied Sec-Fetch-Site must be same-origin. Preserves the session cookie. Permissions and target rules: docs/center/affiliation.md#http-契约.",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID"), ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = Search, content_type = "application/json"),
    responses((status = 200, description = "Exact masked account candidate", body = CandidateReceipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn candidate_by_mobile(
    Extension(actor): Extension<Actor>,
    State(service): State<affiliation::Service>,
    Json(input): Json<Search>,
) -> Response {
    route::query(
        "affiliation_candidate",
        service.candidate_by_mobile(&actor, input.mobile),
        candidate,
    )
    .await
}

#[utoipa::path(get, path = "/affiliations/candidates/{person_id}", operation_id = "affiliationCandidateByPerson",
    description = "Locate another person with institution.affiliations.read or institution.affiliations.correct. Returns only masked identity and target confirmation. No query or body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Supplied Sec-Fetch-Site must be same-origin. Preserves the session cookie. Target rules: docs/center/affiliation.md#http-契约.",
    params(("person_id" = person::Id, Path, description = "Permanent target person"), ("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID"), ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID")),
    responses((status = 200, description = "Exact masked account or permanent-person candidate", body = CandidateReceipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn candidate_by_person(
    Extension(actor): Extension<Actor>,
    Extension(person_id): Extension<person::Id>,
    State(service): State<affiliation::Service>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(error) = validate_empty(body) {
        return error.into_response();
    }
    route::query(
        "affiliation_candidate",
        service.candidate_by_person(&actor, person_id),
        candidate,
    )
    .await
}

fn candidate(candidate: affiliation::Candidate) -> Response {
    let identity = candidate.identity();
    let person_id = identity.person_id();
    let revision_id = identity.revision_id();
    let target = match candidate.target() {
        affiliation::Target::Account(account_id) => Target::Account {
            account_id,
            person_id,
            revision_id,
        },
        affiliation::Target::Person => Target::Person {
            person_id,
            revision_id,
        },
    };
    Json(CandidateReceipt {
        code: "ok",
        candidate: Candidate {
            target,
            name: identity.name(),
        },
    })
    .into_response()
}

#[utoipa::path(get, path = "/persons/{id}/affiliation", operation_id = "personAffiliation",
    description = "Read another person's value and createdAt with institution.affiliations.read. No query or body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Supplied Sec-Fetch-Site must be same-origin. Preserves the session cookie. Visibility and target rules: docs/center/affiliation.md#http-契约.",
    params(("id" = person::Id, Path, description = "Permanent target person"), ("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID"), ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID")),
    responses((status = 200, description = "Current affiliation value and effective time", body = Current),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable or affiliation_unset", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn read_person(
    Extension(actor): Extension<Actor>,
    Extension(person_id): Extension<person::Id>,
    State(service): State<affiliation::Service>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(error) = validate_empty(body) {
        return error.into_response();
    }
    route::query(
        "affiliation_management_read",
        service.read_person(&actor, person_id),
        |affiliation| {
            Json(Current {
                code: "ok",
                affiliation,
            })
            .into_response()
        },
    )
    .await
}

#[utoipa::path(get, path = "/affiliations", operation_id = "affiliationValues",
    description = "Read all 13 fixed affiliation codes and display names in ascending code order for first fill and display. Requires a current valid website session without institutional or Workforce permission. Reads no person's affiliation. No pagination, query or body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Supplied Sec-Fetch-Site must be same-origin. Preserves the session cookie and expiry. Vocabulary: docs/center/values.md.",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID; no persistent read operation"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID selecting its cookie")),
    responses((status = 200, description = "Complete fixed code and display-name vocabulary; no default or unset entry", body = Values),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn values(
    Extension(actor): Extension<Actor>,
    State(service): State<affiliation::Service>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(error) = validate_empty(body) {
        return error.into_response();
    }
    route::query("affiliation_values", service.values(&actor), |codes| {
        Json(Values {
            code: "ok",
            affiliations: codes
                .iter()
                .map(|&value| NamedCode {
                    value,
                    name: value.name(),
                })
                .collect(),
        })
        .into_response()
    })
    .await
}

#[utoipa::path(get, path = "/accounts/current/affiliation", operation_id = "currentAffiliation",
    description = "Read the current person's value and createdAt without institutional or Workforce permission. No query or body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Supplied Sec-Fetch-Site must be same-origin. Preserves the session cookie. Read contract: docs/center/affiliation.md#http-契约.",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID; no persistent read operation"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID selecting its cookie")),
    responses((status = 200, description = "Current affiliation value and effective time without operator identity", body = Current),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "affiliation_unset", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn read(
    Extension(actor): Extension<Actor>,
    State(service): State<affiliation::Service>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(error) = validate_empty(body) {
        return error.into_response();
    }
    route::query("affiliation_read", service.read(&actor), |affiliation| {
        Json(Current {
            code: "ok",
            affiliation,
        })
        .into_response()
    })
    .await
}

#[utoipa::path(get, path = "/accounts/current/affiliations", operation_id = "currentAffiliationHistory",
    description = "Read the current person's paginated history, newest first, with value and createdAt only. No institutional or Workforce permission is required. No body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Supplied Sec-Fetch-Site must be same-origin. Preserves the session cookie. Pagination and visibility: docs/center/affiliation.md#http-契约.",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID; no persistent read operation"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID selecting its cookie"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20")),
    responses((status = 200, description = "Complete self-visible affiliation history page", body = History),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn history(
    Extension(actor): Extension<Actor>,
    State(service): State<affiliation::Service>,
    Query(pagination): Query<Pagination>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(error) = validate_empty(body) {
        return error.into_response();
    }
    route::query(
        "affiliation_history",
        service.history(&actor, pagination),
        |list| {
            Json(History {
                code: "ok",
                count: list.count(),
                affiliations: list.affiliations(),
            })
            .into_response()
        },
    )
    .await
}

#[utoipa::path(get, path = "/persons/{id}/affiliations", operation_id = "personAffiliationHistory",
    description = "Read another person's paginated history, newest first, with institution.affiliations.read. Items include value, createdAt and masked operator identity. No body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Supplied Sec-Fetch-Site must be same-origin. Preserves the session cookie. Pagination and visibility: docs/center/affiliation.md#http-契约.",
    params(("id" = person::Id, Path, description = "Permanent target person UUID"),
        ("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID; no persistent read operation"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID selecting its cookie"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20")),
    responses((status = 200, description = "Qualified affiliation history page with fixed masked operators", body = ManagementHistory),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn history_by_person(
    Extension(actor): Extension<Actor>,
    Extension(person_id): Extension<person::Id>,
    State(service): State<affiliation::Service>,
    Query(pagination): Query<Pagination>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(error) = validate_empty(body) {
        return error.into_response();
    }
    route::query(
        "affiliation_management_history",
        service.history_by_person(&actor, person_id, pagination),
        |list| {
            Json(ManagementHistory {
                code: "ok",
                count: list.count(),
                affiliations: list.affiliations().iter().map(Into::into).collect(),
            })
            .into_response()
        },
    )
    .await
}

#[utoipa::path(put, path = "/accounts/current/affiliation", operation_id = "createCurrentAffiliation",
    description = "Record the current person's first affiliation without institutional or Workforce permission. Requires exact Origin; supplied Sec-Fetch-Site must be same-origin. Success returns only code: ok and preserves the session cookie. Original-operation and conflict rules: docs/center/affiliation.md#http-契约.",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required operation UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = affiliation::Input, content_type = "application/json"),
    responses((status = 200, description = "First affiliation recorded or original completion confirmed", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 409, description = "request_mismatch or state_conflict", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn create(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<affiliation::Service>,
    Json(input): Json<affiliation::Input>,
) -> Response {
    route::command(service.create(actor, operation_id, input), |()| {
        Json(Receipt { code: "ok" }).into_response()
    })
    .await
}

#[utoipa::path(put, path = "/persons/{id}/affiliation", operation_id = "correctPersonAffiliation",
    description = "Append an affiliation for a confirmed other person with institution.affiliations.correct. Requires exact Origin; supplied Sec-Fetch-Site must be same-origin. Success returns only code: ok and preserves the session cookie. Target confirmation, institutional levels and replay rules: docs/center/affiliation.md#http-契约.",
    params(("id" = person::Id, Path, description = "Confirmed permanent target person"),
        ("Operation-Id" = uuid::Uuid, Header, description = "Required operation UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = affiliation::management::Input, content_type = "application/json"),
    responses((status = 200, description = "Correction recorded or original completion confirmed", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "target_changed or request_mismatch", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn correct(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    Extension(person_id): Extension<person::Id>,
    State(service): State<affiliation::Service>,
    Json(input): Json<affiliation::management::Input>,
) -> Response {
    route::command(
        service.correct(actor, operation_id, person_id, input),
        |()| Json(Receipt { code: "ok" }).into_response(),
    )
    .await
}
