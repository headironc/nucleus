//! Website correction, full-identity confirmation and authorized history disclosures.

mod management;

use std::time::Duration;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::{
    correction,
    identity::{
        self,
        current::Actor,
        mobile::Mobile,
        operation,
        person::{self, name::Name, number},
    },
    pagination::Pagination,
};
use serde::{Deserialize, Serialize};
use strum::IntoStaticStr;
use tokio::time::{error::Elapsed, timeout};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{RefOr, Schema},
};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    extension::Extension,
    identity::{Context, current, person::IdNumber},
    json::Json,
    path::Path,
    query::Query,
    response::{Common, Failure},
};

pub(super) fn router(context: &Context) -> OpenApiRouter<Context> {
    OpenApiRouter::new()
        .routes(routes!(candidate_by_mobile))
        .routes(routes!(correct))
        .routes(routes!(history))
        .route_layer(middleware::from_fn_with_state(
            context.clone(),
            current::authenticate,
        ))
        .merge(
            OpenApiRouter::new()
                .routes(routes!(candidate_by_person))
                .routes(routes!(management::history))
                .route_layer(middleware::from_fn_with_state(
                    context.clone(),
                    current::authenticate,
                ))
                .route_layer(middleware::from_fn(target)),
        )
}

async fn target(Path(id): Path<person::Id>, mut request: Request, next: Next) -> Response {
    request.extensions_mut().insert(id);
    next.run(request).await
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = correction::Search)]
struct Search {
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: Mobile,
}

#[derive(Serialize, ToSchema)]
#[schema(as = correction::CandidateReceipt)]
struct CandidateReceipt<'a> {
    code: &'static str,
    candidate: Candidate<'a>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = correction::Candidate)]
struct Candidate<'a> {
    target: &'a correction::Target,
    name: &'a Name,
    id_number: &'a number::IdNumber,
}

#[derive(Serialize, ToSchema)]
#[schema(as = correction::Receipt)]
struct Receipt<'a> {
    code: &'static str,
    correction: &'a identity::correction::Correction,
}

#[derive(Serialize, ToSchema)]
#[schema(as = correction::History)]
struct History<'a> {
    code: &'static str,
    count: i64,
    corrections: Vec<Correction<'a>>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = correction::history::Correction)]
struct Correction<'a> {
    id: identity::correction::Id,
    before: Identity<'a>,
    after: Identity<'a>,
    corrected_at: DateTime<Utc>,
}

impl<'a> From<&'a identity::correction::history::Correction> for Correction<'a> {
    fn from(value: &'a identity::correction::history::Correction) -> Self {
        Self {
            id: value.id(),
            before: Identity::from(value.before()),
            after: Identity::from(value.after()),
            corrected_at: value.corrected_at(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = correction::history::Identity)]
struct Identity<'a> {
    name: &'a Name,
    id_number: IdNumber<'a>,
}

impl<'a> From<&'a identity::correction::history::Identity> for Identity<'a> {
    fn from(value: &'a identity::correction::history::Identity) -> Self {
        Self {
            name: value.name(),
            id_number: IdNumber {
                suffix: value.suffix(),
            },
        }
    }
}

#[utoipa::path(get, path = "/accounts/current/corrections", operation_id = "listCurrentCorrections",
    description = "Returns the original session's own person history without requiring a management role. Records are ordered by personal sequence descending, then ID descending. Before and after names are complete; ID numbers contain only their last four characters. No operator is disclosed. Current identity, count and the complete page share one three-second read-only snapshot. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID; no persistent read operation"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"), ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20")),
    responses((status = 200, description = "Complete self-visible correction history page", body = History), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn history(
    Extension(actor): Extension<Actor>,
    State(context): State<Context>,
    Query(pagination): Query<Pagination>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        context.services.correction().history(&actor, pagination),
    )
    .await
    {
        Ok(Ok(list)) => Json(History {
            code: "ok",
            count: list.count(),
            corrections: list.corrections().iter().map(Correction::from).collect(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == correction::Failure::Unavailable {
                tracing::error!(stage = "correction_history", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "correction_history", kind = "unavailable", error = ?error);
            failed(correction::Failure::Unavailable)
        }
    }
}

#[utoipa::path(post, path = "/corrections/candidates", operation_id = "correctionCandidateByMobile",
    description = "Confirms a formal account by its complete mobile. Requires institution.corrections and another strictly lower institutional account. Returns the full current name and ID number with their fixed person and revision IDs from one snapshot. Requires exact Origin; any supplied Sec-Fetch-Site must be same-origin. This authorized read does not change identity.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = Search, content_type = "application/json"),
    responses((status = 200, description = "Exact full-identity candidate", body = CandidateReceipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn candidate_by_mobile(
    Extension(actor): Extension<Actor>,
    State(context): State<Context>,
    Json(input): Json<Search>,
) -> Response {
    candidate(
        timeout(
            Duration::from_secs(30),
            context
                .services
                .correction()
                .candidate_by_mobile(&actor, input.mobile),
        )
        .await,
    )
}

#[utoipa::path(get, path = "/corrections/{person_id}", operation_id = "correctionCandidateByPerson",
    description = "Confirms a permanent person, including one without a current account. Requires institution.corrections; self and current accounts of equal or higher institutional level remain unavailable. Returns the full current name and ID number with their fixed person and revision IDs from one snapshot. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin.",
    params(("person_id" = person::Id, Path, description = "Permanent target person"), ("Operation-Id" = Uuid, Header, description = "Required request UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID")),
    responses((status = 200, description = "Exact full-identity candidate", body = CandidateReceipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn candidate_by_person(
    Extension(actor): Extension<Actor>,
    Extension(person_id): Extension<person::Id>,
    State(context): State<Context>,
) -> Response {
    candidate(
        timeout(
            Duration::from_secs(30),
            context
                .services
                .correction()
                .candidate_by_person(&actor, person_id),
        )
        .await,
    )
}

fn candidate(
    result: Result<Result<correction::Candidate, correction::Error>, Elapsed>,
) -> Response {
    match result {
        Ok(Ok(value)) => Json(CandidateReceipt {
            code: "ok",
            candidate: Candidate {
                target: value.target(),
                name: value.name(),
                id_number: value.id_number(),
            },
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == correction::Failure::Unavailable {
                tracing::error!(stage = "correction_candidate", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "correction_candidate", kind = "unavailable", error = ?error);
            failed(correction::Failure::Unavailable)
        }
    }
}

#[utoipa::path(post, path = "/corrections", operation_id = "correctIdentity",
    description = "Atomically replaces both complete real-name fields of the confirmed person. Requires institution.corrections, a current normal administrator, and exact Origin; any supplied Sec-Fetch-Site must be same-origin. Current account targets must have a strictly lower institutional level. An identical original operation returns its permanent result before new target/version checks; altered confirmed input returns request_mismatch. Sessions, mobile and roles are unchanged. Changing the ID number of a person with a current account appends an Account projection and advances its state; a name-only correction leaves Account state unchanged. No account is created for a person without one, and a cancelled Account tombstone is unchanged.",
    params(("Operation-Id" = Uuid, Header, description = "Original correction operation UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = correction::Input, content_type = "application/json"),
    responses((status = 201, description = "New permanent correction", body = Receipt), (status = 200, description = "Permanent original correction", body = Receipt), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 409, description = "request_mismatch, target_changed, state_conflict or id_number_taken", body = Failure), (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn correct(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(context): State<Context>,
    Json(input): Json<correction::Input>,
) -> Response {
    crate::route::command(
        context
            .services
            .correction()
            .correct(actor, operation_id, input),
        |value| {
            (
                if value.created() {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                },
                Json(Receipt {
                    code: "ok",
                    correction: value.correction(),
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
}
impl crate::response::AsCode for Code {}
impl PartialSchema for Code {
    fn schema() -> RefOr<Schema> {
        Common::schema()
    }
}
impl ToSchema for Code {}

fn failed(failure: correction::Failure) -> Response {
    let (status, code) = match failure {
        correction::Failure::Invalid => (
            StatusCode::UNAUTHORIZED,
            Code::Common(Common::AuthenticationInvalid),
        ),
        correction::Failure::Denied => (
            StatusCode::FORBIDDEN,
            Code::Common(Common::PermissionDenied),
        ),
        correction::Failure::Absent => (StatusCode::NOT_FOUND, Code::TargetUnavailable),
        correction::Failure::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Code::Common(Common::DependencyUnavailable),
        ),
    };
    (status, Json(Failure { code })).into_response()
}
