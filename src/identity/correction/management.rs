//! Authorized complete correction-history responses, distinct from self disclosure.

use std::time::Duration;

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::{
    correction,
    identity::{
        self,
        correction::history::management,
        current::Actor,
        person::{self, name::Name, number::IdNumber},
    },
    pagination::Pagination,
};
use serde::Serialize;
use tokio::time::timeout;
use utoipa::ToSchema;

use crate::{
    extension::Extension,
    identity::{Context, correction::failed},
    json::Json,
    query::Query,
    response::Failure,
};

#[derive(Serialize, ToSchema)]
#[schema(as = correction::history::management::List)]
struct History<'a> {
    code: &'static str,
    count: i64,
    corrections: Vec<Correction<'a>>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = correction::history::management::Correction)]
struct Correction<'a> {
    id: identity::correction::Id,
    before: Identity<'a>,
    after: Identity<'a>,
    operator: Operator<'a>,
    corrected_at: DateTime<Utc>,
}

impl<'a> From<&'a management::Correction> for Correction<'a> {
    fn from(value: &'a management::Correction) -> Self {
        Self {
            id: value.id(),
            before: Identity::from(value.before()),
            after: Identity::from(value.after()),
            operator: Operator {
                person_id: value.operator().person_id(),
                name: value.operator().name(),
            },
            corrected_at: value.corrected_at(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = correction::history::management::Identity)]
struct Identity<'a> {
    name: &'a Name,
    id_number: &'a IdNumber,
}

impl<'a> From<&'a management::Identity> for Identity<'a> {
    fn from(value: &'a management::Identity) -> Self {
        Self {
            name: value.name(),
            id_number: value.id_number(),
        }
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = correction::history::management::Operator)]
struct Operator<'a> {
    person_id: person::Id,
    name: &'a Name,
}

#[utoipa::path(get, path = "/persons/{id}/corrections", operation_id = "listPersonCorrections",
    description = "Requires current institution.corrections and another person. A target with a current account must have a strictly lower institutional level; a person without a current account requires no level comparison. Returns complete before/after names and ID numbers, and each correction's fixed operator person ID and full name. Records are ordered by personal sequence descending, then ID descending. Authorization, target, count and the complete page share one three-second read-only snapshot. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin.",
    params(("id" = person::Id, Path, description = "Permanent target person"), ("Operation-Id" = Uuid, Header, description = "Required request UUID; no persistent read operation"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("page" = Option<i64>, Query, minimum = 1, description = "Defaults to 1"), ("size" = Option<i64>, Query, minimum = 1, maximum = 100, description = "Defaults to 20")),
    responses((status = 200, description = "Complete authorized correction history page", body = History), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
pub(super) async fn history(
    Extension(actor): Extension<Actor>,
    Extension(person_id): Extension<person::Id>,
    State(context): State<Context>,
    Query(pagination): Query<Pagination>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        context
            .services
            .correction()
            .history_by_person(&actor, person_id, pagination),
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
