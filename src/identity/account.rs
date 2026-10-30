//! Known-account avatar reads and confirmation shared by management operations.

use std::time::Duration;

use axum::{
    body::Bytes,
    extract::{Request, State, rejection::BytesRejection},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use domain::{
    account,
    identity::{account::Id, current::Actor, mobile::Mobile, operation, person, suspension},
};
use serde::{Deserialize, Serialize};
use strum::IntoStaticStr;
use tokio::time::timeout;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{RefOr, Schema},
};
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouterExt},
    routes,
};

use crate::{
    extension::Extension,
    identity::{Context, avatar, current, person::IdNumber},
    json::Json,
    path::Path,
    query::Query,
    response::{Common, Failure},
};

pub(super) fn router(context: &Context) -> OpenApiRouter<Context> {
    OpenApiRouter::new()
        .routes(routes!(candidate))
        .route_layer(middleware::from_fn_with_state(
            context.clone(),
            current::authenticate,
        ))
        .routes(routes!(read_avatar).map(|router| {
            router
                .route_layer(middleware::from_fn_with_state(
                    context.clone(),
                    current::authenticate,
                ))
                .route_layer(middleware::from_fn(target))
        }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Serialize, ToSchema)]
#[schema(as = account::Avatar)]
struct Avatar<'a> {
    code: &'static str,
    avatar: avatar::Current<'a>,
}

#[utoipa::path(get, path = "/accounts/{id}/avatar", operation_id = "accountAvatar",
    description = "Read a known account's current avatar with a valid website session. No institution permission is required; paused targets remain readable. Unknown and cancelled accounts return the same 404. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin.",
    params(("Operation-Id" = Uuid, Header, description = "This read request UUID"),
        ("Session-Id" = Uuid, Header, description = "Selects a currently valid website session cookie"),
        ("id" = Id, Path, description = "Known account UUID")),
    responses((status=200,description="Current avatar revision and signed link",body=Avatar),
        (status=400,description="invalid_input; no query or body is accepted",body=Failure),
        (status=401,description="authentication_invalid",body=Failure),
        (status=403,description="permission_denied",body=Failure),
        (status=404,description="target_unavailable",body=Failure),
        (status=413,description="invalid_input",body=Failure),
        (status=429,description="rate_limited",body=Failure,headers(("Retry-After"=u64,description="Wait seconds"))),
        (status=500,description="internal_error",body=Failure),
        (status=503,description="dependency_unavailable",body=Failure)))]
async fn read_avatar(
    Extension(request_id): Extension<operation::Id>,
    Extension(actor): Extension<Actor>,
    Extension(id): Extension<Id>,
    State(context): State<Context>,
    Query(_): Query<Empty>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let validity = match body {
        Ok(body) if body.is_empty() => Ok(()),
        Ok(_) => Err(StatusCode::BAD_REQUEST),
        Err(error) => Err(error.status()),
    };
    if let Err(status) = validity {
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
        context.services.account().fetch_avatar(&actor, id),
    )
    .await
    {
        Ok(Ok(value)) => Json(Avatar {
            code: "ok",
            avatar: (&value).into(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == account::Failure::Unavailable {
                tracing::error!(operation_id=%request_id, stage="account_avatar", error=?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(operation_id=%request_id, stage="account_avatar", error=?error);
            failed(account::Failure::Unavailable)
        }
    }
}

pub(super) async fn target(Path(id): Path<Id>, mut request: Request, next: Next) -> Response {
    request.extensions_mut().insert(id);
    next.run(request).await
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = account::Search)]
struct Search {
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: Mobile,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = account::Subject)]
pub(crate) struct Subject<'a> {
    account_id: Id,
    person_id: person::Id,
    revision_id: person::revision::Id,
    name: &'a str,
    id_number: IdNumber<'a>,
}

impl<'a> From<&'a domain::identity::account::Subject> for Subject<'a> {
    fn from(subject: &'a domain::identity::account::Subject) -> Self {
        Self {
            account_id: subject.id(),
            person_id: subject.person_id(),
            revision_id: subject.revision_id(),
            name: subject.name(),
            id_number: IdNumber {
                suffix: subject.suffix(),
            },
        }
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = account::Candidate)]
struct Candidate<'a> {
    code: &'static str,
    identity: Subject<'a>,
    #[schema(required)]
    suspension_id: Option<suspension::Id>,
}

#[utoipa::path(post, path = "/accounts/candidates", operation_id = "accountCandidate",
    description = "Precisely confirms another lower-level formal account using its full mobile. Requires institution.accounts and exact Origin; any supplied Sec-Fetch-Site must be same-origin. The nullable suspensionId is the current open suspension. This read supplies confirmation for suspension, resumption, cancellation or assisted mobile change; each operation checks its own current qualification.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID"), ("Session-Id" = Uuid, Header, description = "Required website session UUID"), ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = Search, content_type = "application/json"),
    responses((status = 200, description = "Exact masked candidate", body = Candidate), (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure), (status = 403, description = "permission_denied", body = Failure), (status = 404, description = "target_unavailable", body = Failure), (status = 413, description = "invalid_input", body = Failure), (status = 415, description = "invalid_input", body = Failure), (status = 422, description = "invalid_input", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds"))), (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn candidate(
    Extension(actor): Extension<Actor>,
    State(context): State<Context>,
    Json(input): Json<Search>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        context.services.account().candidate(&actor, input.mobile),
    )
    .await
    {
        Ok(Ok(candidate)) => Json(Candidate {
            code: "ok",
            identity: Subject::from(candidate.identity()),
            suspension_id: candidate.suspension_id(),
        })
        .into_response(),
        Ok(Err(error)) => {
            if error.failure() == account::Failure::Unavailable {
                tracing::error!(stage = "account_candidate", kind = "unavailable", error = ?error);
            }
            failed(error.failure())
        }
        Err(error) => {
            tracing::error!(stage = "account_candidate", kind = "unavailable", error = ?error);
            failed(account::Failure::Unavailable)
        }
    }
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

fn failed(failure: account::Failure) -> Response {
    let (status, code) = match failure {
        account::Failure::Invalid => (
            StatusCode::UNAUTHORIZED,
            Code::Common(Common::AuthenticationInvalid),
        ),
        account::Failure::Denied => (
            StatusCode::FORBIDDEN,
            Code::Common(Common::PermissionDenied),
        ),
        account::Failure::Absent => (StatusCode::NOT_FOUND, Code::TargetUnavailable),
        account::Failure::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Code::Common(Common::DependencyUnavailable),
        ),
    };
    (status, Json(Failure { code })).into_response()
}
