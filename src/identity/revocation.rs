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
                current::authenticate,
            ))
        }))
        .routes(routes!(revoke_all).map(|router| {
            router.route_layer(middleware::from_fn_with_state(
                context.clone(),
                current::authenticate,
            ))
        }))
        .routes(routes!(read).map(|router| {
            router.route_layer(middleware::from_fn_with_state(
                context.clone(),
                current::authenticate,
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
    description = "Revokes the currently authenticated website session and returns its permanent logout ID, count and time. Requires a current valid website session and exact Origin. Any supplied Sec-Fetch-Site must be same-origin. Success clears only the authenticated session cookie. The revoked session cannot replay DELETE; a new valid session of the same account can read the original result with GET /revocations.",
    params(("Operation-Id" = Uuid, Header, description = "Original logout operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Selects the website session cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
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
    description = "Revokes all retained unrevoked sessions of the currently authenticated account, including expired sessions, and returns the permanent logout ID, count and time for that fixed set. Requires a current valid website session and exact Origin. Any supplied Sec-Fetch-Site must be same-origin. Success clears only the authenticated session cookie. Later sessions are not added to the original set; a new valid session of the same account can read it with GET /revocations.",
    params(("Operation-Id" = Uuid, Header, description = "Original logout operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Selects the website session cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
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
    crate::route::command(
        context
            .services
            .revocation()
            .revoke(actor, operation_id, scope),
        |result| {
            let cookie = match crate::identity::session::clear(id) {
                Ok(cookie) => cookie,
                Err(cause) => {
                    tracing::error!(%operation_id,stage="revocation_cookie",kind="internal",error=?cause);
                    return failed(StatusCode::INTERNAL_SERVER_ERROR, Common::InternalError);
                }
            };
            (
                [(header::SET_COOKIE, cookie)],
                Json(Receipt {
                    code: "ok",
                    completion: Completion {
                        id: operation_id,
                        count: result.count().get(),
                        revoked_at: result.revoked_at(),
                    },
                }),
            ).into_response()
        },
    )
    .await
}

#[utoipa::path(get, path="/revocations", operation_id="readRevocations",
    description = "Requires a current valid website session of the original account. Reads the permanent original logout's scope, initiatorId and complete fixed session set ordered by session ID, even after short-term session cleanup. The required original object binds scope and initiatorId for a nonempty set; missing and other-account operations return original: null and an empty list. This read neither executes logout nor sets a cookie. Origin must match the configured origin when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin.",
    params(("Operation-Id"=Uuid,Header,description="This read request UUID"),
        ("Session-Id"=Uuid,Header,description="Selects a currently valid website session cookie"),
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
