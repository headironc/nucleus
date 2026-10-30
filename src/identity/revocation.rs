//! Website logout and a separate read of the original account-scoped completion.

use axum::{
    body::Bytes,
    extract::{State, rejection::BytesRejection},
    http::{StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use domain::identity::{current::Actor, operation, revocation, session};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouterExt},
    routes,
};

use crate::{
    extension::Extension,
    identity::{Context, current},
    json::Json,
    query::Query,
    response::{Common, Failure},
};

pub(super) fn router(context: &Context) -> OpenApiRouter<Context> {
    OpenApiRouter::new()
        .routes(routes!(revoke).map(|router| {
            router.route_layer(middleware::from_fn_with_state(
                context.clone(),
                current::authenticate_mobile,
            ))
        }))
        .routes(routes!(revoke_all).map(|router| {
            router.route_layer(middleware::from_fn_with_state(
                context.clone(),
                current::authenticate_mobile,
            ))
        }))
        .routes(routes!(read).map(|router| {
            router.route_layer(middleware::from_fn_with_state(
                context.clone(),
                current::authenticate_mobile,
            ))
        }))
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = revocation::Completion)]
struct Completion {
    id: operation::Id,
    count: u64,
    revoked_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = revocation::Receipt)]
struct Receipt {
    code: &'static str,
    completion: Completion,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    operation_id: operation::Id,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = revocation::Revocation)]
struct Revocation {
    id: session::Id,
    revoked_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = revocation::Scope)]
enum Scope {
    Current,
    All,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = revocation::Original)]
struct Original {
    scope: Scope,
    initiator_id: session::Id,
}

#[derive(Serialize, ToSchema)]
#[schema(as = revocation::Collection)]
struct Collection {
    code: &'static str,
    #[schema(required = true)]
    original: Option<Original>,
    count: usize,
    revocations: Vec<Revocation>,
}

#[utoipa::path(delete, path = "/sessions/current", operation_id = "revokeCurrentSession",
    description = "Revokes the currently authenticated session. Website cookies require exact Origin and same-origin Sec-Fetch-Site when supplied. Mini-program Bearer requires no website Origin and cannot be mixed with Session-Id or a session cookie. Only website success clears its selected cookie. A revoked proof cannot replay DELETE; a new session of the same account can read the original with GET /revocations.",
    params(("Operation-Id" = Uuid, Header, description = "Original logout operation UUID"),
        ("Session-Id" = Option<Uuid>, Header, description = "Website cookie mode only"),
        ("Authorization" = Option<String>, Header, description = "Opaque mini-program Bearer token; exclusive with website credentials")),
    responses((status=200,description="Current session revoked",body=Receipt,headers(("Set-Cookie"=String,description="Expires only the selected session cookie"))),
        (status=400,description="invalid_input; no request body is accepted",body=Failure),
        (status=401,description="authentication_invalid",body=Failure),
        (status=403,description="permission_denied",body=Failure),
        (status=409,description="request_mismatch",body=Failure),
        (status=413,description="invalid_input",body=Failure),
        (status=429,description="rate_limited",body=Failure,headers(("Retry-After"=u64,description="Wait seconds before another attempt"))),
        (status=500,description="internal_error",body=Failure),
        (status=503,description="dependency_unavailable or outcome_unknown",body=Failure)))]
async fn revoke(
    Extension(operation_id): Extension<operation::Id>,
    Extension(actor): Extension<Actor>,
    State(context): State<Context>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    perform(
        operation_id,
        actor,
        context,
        body,
        revocation::Scope::Current,
    )
    .await
}

#[utoipa::path(delete, path = "/sessions", operation_id = "revokeAllSessions",
    description = "Revokes every retained unrevoked session of the authenticated account across website and mini-program sources, including expired sessions. Accepts a selected website cookie with exact Origin, or a unique mini-program Bearer token without website Origin. Mixed credentials are rejected. Only website success clears its selected cookie. Later sessions are not added; a new valid session of the same account can read the original with GET /revocations.",
    params(("Operation-Id" = Uuid, Header, description = "Original logout operation UUID"),
        ("Session-Id" = Option<Uuid>, Header, description = "Website cookie mode only"),
        ("Authorization" = Option<String>, Header, description = "Opaque mini-program Bearer token; exclusive with website credentials")),
    responses((status=200,description="All retained unrevoked account sessions revoked",body=Receipt,headers(("Set-Cookie"=String,description="Expires only the selected session cookie"))),
        (status=400,description="invalid_input; no request body is accepted",body=Failure),
        (status=401,description="authentication_invalid",body=Failure),
        (status=403,description="permission_denied",body=Failure),
        (status=409,description="request_mismatch",body=Failure),
        (status=413,description="invalid_input",body=Failure),
        (status=429,description="rate_limited",body=Failure,headers(("Retry-After"=u64,description="Wait seconds before another attempt"))),
        (status=500,description="internal_error",body=Failure),
        (status=503,description="dependency_unavailable or outcome_unknown",body=Failure)))]
async fn revoke_all(
    Extension(operation_id): Extension<operation::Id>,
    Extension(actor): Extension<Actor>,
    State(context): State<Context>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    perform(operation_id, actor, context, body, revocation::Scope::All).await
}

async fn perform(
    operation_id: operation::Id,
    actor: Actor,
    context: Context,
    body: Result<Bytes, BytesRejection>,
    scope: revocation::Scope,
) -> Response {
    if let Err(status) = empty(body) {
        return failed(status, Common::InvalidInput);
    }
    let id = actor.session_id();
    let cookie = actor.origin().transport() == "cookie";
    crate::route::command(
        context
            .services
            .revocation()
            .revoke(actor, operation_id, scope),
        |result| {
            let receipt = Json(Receipt {
                code: "ok",
                completion: Completion { id: operation_id, count: result.count().get(), revoked_at: result.revoked_at() },
            });
            if !cookie { return receipt.into_response(); }
            let cookie = match crate::identity::session::clear(id) {
                Ok(cookie) => cookie,
                Err(cause) => {
                    tracing::error!(%operation_id,stage="revocation_cookie",kind="internal",error=?cause);
                    return failed(StatusCode::INTERNAL_SERVER_ERROR, Common::InternalError);
                }
            };
            (
                [(header::SET_COOKIE, cookie)],
                receipt,
            ).into_response()
        },
    )
    .await
}

#[utoipa::path(get, path="/revocations", operation_id="readRevocations",
    description = "Requires a valid website cookie or mini-program Bearer session of the original account. Reads the permanent logout scope, initiatorId and fixed session set in session-ID order, even after session cleanup. Missing and other-account operations return original: null and an empty list. This read never executes logout or sets a cookie. Website Origin must match when supplied; absent Origin requires same-origin Sec-Fetch-Site. Bearer does not require website Origin and cannot be mixed with website credentials.",
    params(("Operation-Id"=Uuid,Header,description="This read request UUID"),
        ("Session-Id"=Option<Uuid>,Header,description="Website cookie mode only"),
        ("Authorization"=Option<String>,Header,description="Opaque mini-program Bearer token; exclusive with website credentials"),
        ("operation_id"=Uuid,Query,description="Original logout UUID")),
    responses((status=200,description="Original account-scoped logout set",body=Collection),
        (status=400,description="invalid_input",body=Failure),(status=401,description="authentication_invalid",body=Failure),
        (status=403,description="permission_denied",body=Failure),(status=413,description="invalid_input",body=Failure),
        (status=429,description="rate_limited",body=Failure,headers(("Retry-After"=u64,description="Wait seconds before another attempt"))),(status=500,description="internal_error",body=Failure),
        (status=503,description="dependency_unavailable",body=Failure)))]
async fn read(
    Extension(actor): Extension<Actor>,
    State(context): State<Context>,
    Query(input): Query<Input>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if let Err(status) = empty(body) {
        return failed(status, Common::InvalidInput);
    }
    crate::route::query(
        "revocation_read",
        context
            .services
            .revocation()
            .read(&actor, input.operation_id),
        |result| {
            let original = result.as_ref().map(|result| Original {
                scope: match result.scope() {
                    revocation::Scope::Current => Scope::Current,
                    revocation::Scope::All => Scope::All,
                },
                initiator_id: result.initiator_id(),
            });
            let revocations: Vec<_> = result
                .as_ref()
                .map(|result| result.revocations())
                .unwrap_or_default()
                .iter()
                .map(|result| Revocation {
                    id: result.id(),
                    revoked_at: result.revoked_at(),
                })
                .collect();
            Json(Collection {
                code: "ok",
                original,
                count: revocations.len(),
                revocations,
            })
            .into_response()
        },
    )
    .await
}

fn empty(body: Result<Bytes, BytesRejection>) -> Result<(), StatusCode> {
    match body {
        Ok(body) if body.is_empty() => Ok(()),
        Ok(_) => Err(StatusCode::BAD_REQUEST),
        Err(error) => Err(error.status()),
    }
}

fn failed(status: StatusCode, code: Common) -> Response {
    (status, Json(Failure { code })).into_response()
}
