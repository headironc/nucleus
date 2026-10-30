//! Allocation, ordinary correction and current reading of permanent Workforce numbers.

pub(super) mod application;
pub(super) mod directory;
pub(crate) mod settings;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Request, State, rejection::BytesRejection},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::{
    access::grant,
    identity::{current::Actor, mobile::Mobile, operation, person},
    pagination::Pagination,
    workforce::number,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    extension::Extension,
    identity::{self, account::Subject},
    json::Json,
    path::Path,
    query::Query,
    response::{Common, Failure},
};

pub(super) fn router(service: number::Service, authentication: identity::Context) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(allocate))
        .routes(routes!(candidate))
        .routes(routes!(correction_candidate))
        .routes(routes!(correction_holder))
        .routes(routes!(recovery_candidate))
        .routes(routes!(restore))
        .routes(routes!(read))
        .route_layer(middleware::from_fn_with_state(
            authentication.clone(),
            identity::current::authenticate,
        ))
        .merge(
            OpenApiRouter::new()
                .routes(routes!(assist))
                .routes(routes!(supplement))
                .routes(routes!(correct))
                .routes(routes!(list_revisions))
                .route_layer(middleware::from_fn_with_state(
                    authentication,
                    identity::current::authenticate,
                ))
                .route_layer(middleware::from_fn(target)),
        )
        .layer(DefaultBodyLimit::max(8192))
        .with_state(service)
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = number::Search)]
struct Search {
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: Mobile,
}

#[derive(Serialize, ToSchema)]
#[schema(as = number::Subject)]
struct Confirmation<'a> {
    #[serde(flatten)]
    identity: Subject<'a>,
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: &'a str,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = number::Candidate)]
struct Candidate<'a> {
    code: &'static str,
    identity: Confirmation<'a>,
    grant_id: grant::Id,
}

#[utoipa::path(post, path = "/workforce/numbers/candidates", operation_id = "workforceNumberCandidate",
    description = "Find a member eligible for first number allocation by exact mobile. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口).",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = Search, content_type = "application/json"),
    responses((status = 200, description = "Masked target confirmation and current Grant", body = Candidate),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "state_conflict or number_present", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn candidate(
    Extension(actor): Extension<Actor>,
    State(service): State<number::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<Search>,
) -> Response {
    crate::route::query(
        "number_candidate",
        service.candidate(&actor, input.mobile),
        |candidate| {
            Json(Candidate {
                code: "ok",
                identity: Confirmation {
                    identity: Subject::from(candidate.identity()),
                    mobile: candidate.mobile().as_ref(),
                },
                grant_id: candidate.grant_id(),
            })
            .into_response()
        },
    )
    .await
}

#[derive(Serialize, ToSchema)]
#[schema(as = number::CorrectionCandidate)]
struct CorrectionCandidate<'a> {
    code: &'static str,
    identity: Confirmation<'a>,
    number: Revision,
}

#[utoipa::path(post, path = "/workforce/numbers/corrections/candidates", operation_id = "workforceNumberCorrectionCandidate",
    description = "Find a correction target and current number by exact mobile. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口).",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = Search, content_type = "application/json"),
    responses((status = 200, description = "Masked target confirmation and current number revision", body = CorrectionCandidate),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "target_unavailable; target has no current number", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn correction_candidate(
    Extension(actor): Extension<Actor>,
    State(service): State<number::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<Search>,
) -> Response {
    crate::route::query(
        "number_correction_candidate",
        service.correction_candidate(&actor, input.mobile),
        |candidate| {
            Json(CorrectionCandidate {
                code: "ok",
                identity: Confirmation {
                    identity: Subject::from(candidate.identity()),
                    mobile: candidate.mobile().as_ref(),
                },
                number: Revision::from(candidate.number()),
            })
            .into_response()
        },
    )
    .await
}

#[derive(Serialize, ToSchema)]
#[schema(as = number::CorrectionHolder)]
struct CorrectionHolder<'a> {
    code: &'static str,
    identity: identity::person::Identity<'a>,
    number: Revision,
}

#[utoipa::path(post, path = "/workforce/numbers/corrections/holders", operation_id = "workforceNumberCorrectionHolder",
    description = "Find the current holder of a complete number. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口).",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = number::Number, content_type = "application/json"),
    responses((status = 200, description = "Masked holder confirmation and current number revision", body = CorrectionHolder),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn correction_holder(
    Extension(actor): Extension<Actor>,
    State(service): State<number::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<number::Number>,
) -> Response {
    crate::route::query(
        "number_correction_holder",
        service.correction_holder(&actor, input),
        |holder| {
            Json(CorrectionHolder {
                code: "ok",
                identity: identity::person::Identity::from(holder.identity()),
                number: Revision::from(holder.number()),
            })
            .into_response()
        },
    )
    .await
}

#[derive(Serialize, ToSchema)]
#[schema(as = number::Revision)]
struct Revision {
    id: number::Id,
    subtype: number::Subtype,
    year: i32,
    serial: i16,
}

impl From<&number::Revision> for Revision {
    fn from(revision: &number::Revision) -> Self {
        let number = revision.number();
        Self {
            id: revision.id(),
            subtype: number.subtype(),
            year: number.year(),
            serial: number.serial(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = number::Receipt)]
struct Receipt {
    code: &'static str,
    number: Revision,
}

#[utoipa::path(post, path = "/workforce/numbers", operation_id = "allocateWorkforceNumber",
    description = "Allocate this member's first number for the current year. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口).",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Original allocation UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = number::Input, content_type = "application/json"),
    responses((status = 201, description = "Original fixed revision with its complete number", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 409, description = "number_present, number_pool_exhausted or request_mismatch", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn allocate(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<number::Service>,
    Json(input): Json<number::Input>,
) -> Response {
    crate::route::command(service.allocate(actor, operation_id, input), |revision| {
        (
            StatusCode::CREATED,
            Json(Receipt {
                code: "ok",
                number: Revision::from(&revision),
            }),
        )
            .into_response()
    })
    .await
}

async fn target(Path(id): Path<person::Id>, mut request: Request, next: Next) -> Response {
    request.extensions_mut().insert(id);
    next.run(request).await
}

#[utoipa::path(post, path = "/workforce/persons/{id}/numbers", operation_id = "assistWorkforceNumber",
    description = "Allocate a confirmed member's first number for an earlier entry year. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口).",
    params(("id" = person::Id, Path, description = "The confirmed target person"),
        ("Operation-Id" = uuid::Uuid, Header, description = "Original allocation UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = number::Assistance, content_type = "application/json"),
    responses((status = 201, description = "Original fixed revision with its complete number", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "request_mismatch, target_changed, state_conflict, number_present or number_pool_exhausted", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn assist(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<number::Service>,
    Extension(id): Extension<person::Id>,
    Query(_): Query<Empty>,
    Json(input): Json<number::Assistance>,
) -> Response {
    crate::route::command(
        service.allocate_member(actor, operation_id, id, input),
        |revision| {
            (
                StatusCode::CREATED,
                Json(Receipt {
                    code: "ok",
                    number: Revision::from(&revision),
                }),
            )
                .into_response()
        },
    )
    .await
}

#[utoipa::path(post, path = "/workforce/persons/{id}/supplements", operation_id = "supplementWorkforceNumber",
    description = "Supplement a confirmed member's verified historical number. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口).",
    params(("id" = person::Id, Path, description = "The confirmed target person"),
        ("Operation-Id" = uuid::Uuid, Header, description = "Original supplement UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = number::Supplement, content_type = "application/json"),
    responses((status = 201, description = "Original fixed revision with its complete number", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "request_mismatch, target_changed, state_conflict, number_present or number_occupied", body = Failure),
        (status = 413, description = "invalid_input; body exceeds 8 KiB", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn supplement(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<number::Service>,
    Extension(id): Extension<person::Id>,
    Query(_): Query<Empty>,
    Json(input): Json<number::Supplement>,
) -> Response {
    crate::route::command(
        service.supplement(actor, operation_id, id, input),
        |revision| {
            (
                StatusCode::CREATED,
                Json(Receipt {
                    code: "ok",
                    number: Revision::from(&revision),
                }),
            )
                .into_response()
        },
    )
    .await
}

#[utoipa::path(post, path = "/workforce/persons/{id}/corrections", operation_id = "correctWorkforceNumber",
    description = "Correct the confirmed latest number revision. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口).",
    params(("id" = person::Id, Path, description = "Target person"),
        ("Operation-Id" = uuid::Uuid, Header, description = "Original correction UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = number::Correction, content_type = "application/json"),
    responses((status = 200, description = "Original corrected revision with its complete number", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "target_changed, number_occupied or request_mismatch", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn correct(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<number::Service>,
    Extension(id): Extension<person::Id>,
    Query(_): Query<Empty>,
    Json(input): Json<number::Correction>,
) -> Response {
    crate::route::command(
        service.correct(actor, operation_id, id, input),
        |revision| {
            Json(Receipt {
                code: "ok",
                number: Revision::from(&revision),
            })
            .into_response()
        },
    )
    .await
}

#[derive(Serialize, ToSchema)]
#[schema(as = number::recovery::Candidate)]
struct RecoveryCandidate<'a> {
    code: &'static str,
    identity: Confirmation<'a>,
    #[schema(required)]
    number: Option<Revision>,
}

#[utoipa::path(post, path = "/workforce/recoveries/candidates", operation_id = "workforceRecoveryCandidate",
    description = "Find a recovery beneficiary and their current number by exact mobile. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口).",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = Search, content_type = "application/json"),
    responses((status = 200, description = "Masked beneficiary confirmation and current number or null", body = RecoveryCandidate),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn recovery_candidate(
    Extension(actor): Extension<Actor>,
    State(service): State<number::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<Search>,
) -> Response {
    crate::route::query(
        "number_recovery_candidate",
        service.recovery_candidate(&actor, input.mobile),
        |candidate| {
            Json(RecoveryCandidate {
                code: "ok",
                identity: Confirmation {
                    identity: Subject::from(candidate.identity()),
                    mobile: candidate.mobile().as_ref(),
                },
                number: candidate.number().map(Revision::from),
            })
            .into_response()
        },
    )
    .await
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = number::recovery::Revoked)]
struct Revoked {
    id: number::recovery::RevocationId,
    person_id: person::Id,
    number: Revision,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = number::recovery::Restored)]
struct Restored {
    id: number::recovery::RecoveryId,
    person_id: person::Id,
    origin: number::Id,
    number: Revision,
    revocations: Vec<Revoked>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = number::recovery::Receipt)]
struct RecoveryReceipt {
    code: &'static str,
    recovery: Restored,
}

#[utoipa::path(post, path = "/workforce/recoveries", operation_id = "recoverWorkforceNumber",
    description = "Atomically revoke the confirmed occupier and restore the beneficiary's original number. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口).",
    params(("Operation-Id" = uuid::Uuid, Header, description = "Original recovery UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = number::recovery::Recovery, content_type = "application/json"),
    responses((status = 200, description = "Original recovery with its new revision and revocations, occupier first", body = RecoveryReceipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure),
        (status = 409, description = "target_changed or request_mismatch", body = Failure),
        (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure),
        (status = 422, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable or outcome_unknown", body = Failure)))]
async fn restore(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(service): State<number::Service>,
    Query(_): Query<Empty>,
    Json(input): Json<number::recovery::Recovery>,
) -> Response {
    crate::route::command(service.restore(actor, operation_id, input), |restored| {
        Json(RecoveryReceipt {
            code: "ok",
            recovery: Restored {
                id: restored.id(),
                person_id: restored.person_id(),
                origin: restored.origin(),
                number: Revision::from(restored.number()),
                revocations: restored
                    .revocations()
                    .iter()
                    .map(|revoked| Revoked {
                        id: revoked.id(),
                        person_id: revoked.person_id(),
                        number: Revision::from(revoked.number()),
                    })
                    .collect(),
            },
        })
        .into_response()
    })
    .await
}

/// The current number, or null when the person holds none.
#[derive(Serialize, ToSchema)]
#[schema(as = number::Current)]
struct Current {
    code: &'static str,
    #[schema(required)]
    number: Option<Revision>,
}

fn validate_empty(body: Result<Bytes, BytesRejection>) -> Result<(), StatusCode> {
    match body {
        Ok(body) if body.is_empty() => Ok(()),
        Err(error) => Err(error.status()),
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

#[derive(Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[schema(as = number::HistoryItem)]
enum HistoryItem<'a> {
    #[serde(rename_all = "camelCase")]
    Allocation {
        id: number::history::Id,
        source: number::Source,
        number: Revision,
        time: DateTime<Utc>,
        sequence: i64,
        operation_id: operation::Id,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<&'a str>,
        operator: crate::identity::person::Identity<'a>,
    },
    #[serde(rename_all = "camelCase")]
    Correction {
        id: number::history::Id,
        before: Revision,
        after: Revision,
        time: DateTime<Utc>,
        sequence: i64,
        operation_id: operation::Id,
        reason: &'a str,
        operator: crate::identity::person::Identity<'a>,
    },
    #[serde(rename_all = "camelCase")]
    Recovery {
        id: number::history::Id,
        number: Revision,
        origin: number::Id,
        time: DateTime<Utc>,
        sequence: i64,
        operation_id: operation::Id,
        reason: &'a str,
        operator: crate::identity::person::Identity<'a>,
    },
    #[serde(rename_all = "camelCase")]
    Revocation {
        id: number::history::Id,
        number: Revision,
        recovery_id: number::recovery::RecoveryId,
        time: DateTime<Utc>,
        sequence: i64,
        operation_id: operation::Id,
        reason: &'a str,
        operator: crate::identity::person::Identity<'a>,
    },
}

impl<'a> From<&'a number::HistoryItem> for HistoryItem<'a> {
    fn from(item: &'a number::HistoryItem) -> Self {
        match item.kind() {
            number::HistoryKind::Allocation {
                source,
                number,
                reason,
            } => Self::Allocation {
                id: item.id(),
                source: *source,
                number: Revision::from(number),
                time: item.time(),
                sequence: item.sequence(),
                operation_id: item.operation_id(),
                reason: reason.as_deref(),
                operator: item.operator().into(),
            },
            number::HistoryKind::Correction {
                before,
                after,
                reason,
            } => Self::Correction {
                id: item.id(),
                before: Revision::from(before),
                after: Revision::from(after),
                time: item.time(),
                sequence: item.sequence(),
                operation_id: item.operation_id(),
                reason: reason.as_str(),
                operator: item.operator().into(),
            },
            number::HistoryKind::Recovery {
                number,
                origin,
                reason,
            } => Self::Recovery {
                id: item.id(),
                number: Revision::from(number),
                origin: *origin,
                time: item.time(),
                sequence: item.sequence(),
                operation_id: item.operation_id(),
                reason: reason.as_str(),
                operator: item.operator().into(),
            },
            number::HistoryKind::Revocation {
                number,
                recovery,
                reason,
            } => Self::Revocation {
                id: item.id(),
                number: Revision::from(number),
                recovery_id: *recovery,
                time: item.time(),
                sequence: item.sequence(),
                operation_id: item.operation_id(),
                reason: reason.as_str(),
                operator: item.operator().into(),
            },
        }
    }
}

#[derive(Serialize, ToSchema)]
#[schema(as = number::HistoryList)]
struct HistoryList<'a> {
    code: &'static str,
    count: i64,
    revisions: Vec<HistoryItem<'a>>,
}

#[utoipa::path(get, path = "/workforce/persons/{id}/revisions", operation_id = "listWorkforceNumberRevisions",
    description = "Read a person's permanent number history in object sequence order. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口).",
    params(
        ("id" = person::Id, Path, description = "The target person"),
        ("Operation-Id" = uuid::Uuid, Header, description = "Required request UUID"),
        ("Session-Id" = uuid::Uuid, Header, description = "Required website session UUID"),
        ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"),
        ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20"),
    ),
    responses(
        (status = 200, description = "Complete number history page", body = HistoryList),
        (status = 400, description = "invalid_input", body = Failure),
        (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure),
        (status = 404, description = "target_unavailable", body = Failure),
        (status = 413, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure),
        (status = 503, description = "dependency_unavailable", body = Failure),
    )
)]
async fn list_revisions(
    Extension(actor): Extension<Actor>,
    Extension(id): Extension<person::Id>,
    State(service): State<number::Service>,
    Query(pagination): Query<Pagination>,
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
    crate::route::query(
        "number_history",
        service.history(&actor, id, pagination),
        |list| {
            Json(HistoryList {
                code: "ok",
                count: list.count(),
                revisions: list.revisions().iter().map(HistoryItem::from).collect(),
            })
            .into_response()
        },
    )
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[utoipa::path(get, path = "/workforce/current/numbers", operation_id = "currentWorkforceNumber",
    description = "Read this person's current number or null. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口).",
    params(("Operation-Id" = uuid::Uuid, Header, description = "This read request UUID"), ("Session-Id" = uuid::Uuid, Header, description = "Selects a currently valid website session cookie")),
    responses((status = 200, description = "Current fixed revision with its complete number, or null", body = Current),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 413, description = "invalid_input", body = Failure),
        (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))),
        (status = 500, description = "internal_error", body = Failure), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn read(
    Extension(actor): Extension<Actor>,
    State(service): State<number::Service>,
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
    crate::route::query("number_read", service.read_current(&actor), |revision| {
        Json(Current {
            code: "ok",
            number: revision.as_ref().map(Revision::from),
        })
        .into_response()
    })
    .await
}
