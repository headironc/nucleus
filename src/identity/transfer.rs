//! Website mobile changes and status disclosure to the person or a current manager.

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
    identity::{self, current::Actor, message, mobile::Mobile, operation, transfer},
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
    identity::{Context, current},
    json::Json,
    path::Path,
    query::Query,
    response::{self, Common, Failure},
};

pub(super) fn router(context: &Context) -> OpenApiRouter<Context> {
    OpenApiRouter::new()
        .routes(routes!(assist).map(|router| {
            router
                .route_layer(middleware::from_fn_with_state(
                    context.clone(),
                    current::authenticate,
                ))
                .route_layer(middleware::from_fn(crate::identity::account::target))
        }))
        .routes(routes!(complete_assisted).map(|router| {
            router
                .route_layer(middleware::from_fn_with_state(
                    context.clone(),
                    current::authenticate,
                ))
                .route_layer(middleware::from_fn(crate::identity::account::target))
        }))
        .routes(routes!(create).map(|router| {
            router.route_layer(middleware::from_fn_with_state(
                context.clone(),
                current::authenticate,
            ))
        }))
        .routes(routes!(complete).map(|router| {
            router.route_layer(middleware::from_fn_with_state(
                context.clone(),
                current::authenticate,
            ))
        }))
        .routes(routes!(read).map(|router| {
            router.route_layer(middleware::from_fn(target)).route_layer(
                middleware::from_fn_with_state(context.clone(), current::authenticate),
            )
        }))
}

pub(super) async fn target(
    Path(id): Path<transfer::Id>,
    mut request: Request,
    next: Next,
) -> Response {
    request.extensions_mut().insert(id);
    next.run(request).await
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = transfer::Input)]
struct Input {
    message_id: message::Id,
    #[schema(value_type = String, pattern = "^[0-9]{6}$")]
    code: message::Code,
}

#[derive(Serialize, ToSchema)]
#[schema(as = transfer::Receipt)]
struct Receipt {
    code: &'static str,
    transfer: transfer::Transfer,
}

#[utoipa::path(post, path = "/accounts/{id}/mobile-changes", operation_id = "createAssistedMobileTransfer",
    description = "Establish a mobile change after explicitly verifying another person. Requires current institution.accounts, a strictly lower institutional target and an unchanged account confirmation. Paused targets are eligible. Replaying the exact original input returns the same transfer while current management qualification still holds.",
    params(("id" = identity::account::Id, Path, description = "Confirmed target account"),
        ("Operation-Id" = Uuid, Header, description = "Original transfer operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Selects the manager's website session cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = account::transfer::Input, content_type = "application/json"),
    responses((status=201,description="Established transfer ID and fixed expiry",body=Receipt),
        (status=400,description="invalid_input",body=Failure),
        (status=401,description="authentication_invalid",body=Failure),
        (status=403,description="permission_denied",body=Failure),
        (status=404,description="target_unavailable",body=Failure),
        (status=409,description="request_mismatch or target_changed",body=Failure),
        (status=413,description="invalid_input; body exceeds 8192 bytes",body=Failure),
        (status=415,description="invalid_input",body=Failure),
        (status=422,description="invalid_input; verified must be true",body=Failure),
        (status=429,description="rate_limited",body=Failure,headers(("Retry-After"=u64,description="Wait seconds"))),
        (status=500,description="internal_error",body=Failure),
        (status=503,description="dependency_unavailable or outcome_unknown",body=Failure)))]
async fn assist(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    Extension(account_id): Extension<identity::account::Id>,
    State(context): State<Context>,
    Json(input): Json<account::transfer::Input>,
) -> Response {
    match timeout(
        Duration::from_secs(30),
        context
            .services
            .account()
            .create_transfer(actor, operation_id, account_id, input),
    )
    .await
    {
        Ok(Ok(transfer)) => (
            StatusCode::CREATED,
            Json(Receipt {
                code: "ok",
                transfer,
            }),
        )
            .into_response(),
        Ok(Err(error)) => assisted_failure(&error),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Failure {
                code: Common::OutcomeUnknown,
            }),
        )
            .into_response(),
    }
}

fn assisted_failure(error: &account::transfer::Error) -> Response {
    let (status, code): (_, Code) = match error.failure() {
        account::transfer::Failure::Invalid => (
            StatusCode::UNAUTHORIZED,
            Common::AuthenticationInvalid.into(),
        ),
        account::transfer::Failure::Denied => {
            (StatusCode::FORBIDDEN, Common::PermissionDenied.into())
        }
        account::transfer::Failure::Absent => (StatusCode::NOT_FOUND, Code::TargetUnavailable),
        account::transfer::Failure::TargetChanged => (StatusCode::CONFLICT, Code::TargetChanged),
        account::transfer::Failure::Mismatch => {
            (StatusCode::CONFLICT, Common::RequestMismatch.into())
        }
        account::transfer::Failure::Unknown => (
            StatusCode::SERVICE_UNAVAILABLE,
            Common::OutcomeUnknown.into(),
        ),
        account::transfer::Failure::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Common::DependencyUnavailable.into(),
        ),
    };
    (status, Json(Failure { code })).into_response()
}

#[utoipa::path(post, path = "/mobile-changes", operation_id = "createMobileTransfer",
    description = "Prove the current account phone with its latest original-phone code. A successful transfer lasts ten minutes and is bound to this website session. Replaying the original input neither creates another transfer nor counts a wrong code again.",
    params(("Operation-Id" = Uuid, Header, description = "Original transfer operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Selects the website session cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = Input, content_type = "application/json"),
    responses((status=201,description="Established transfer ID and fixed expiry",body=Receipt),
        (status=400,description="invalid_input",body=Failure),
        (status=401,description="authentication_invalid",body=Failure),
        (status=403,description="verification_invalid or permission_denied",body=Failure),
        (status=409,description="request_mismatch",body=Failure),
        (status=410,description="verification_required",body=Failure),
        (status=413,description="invalid_input; body exceeds 8192 bytes",body=Failure),
        (status=415,description="invalid_input",body=Failure),
        (status=422,description="invalid_input",body=Failure),
        (status=429,description="rate_limited",body=Failure,headers(("Retry-After"=u64,description="Wait seconds"))),
        (status=500,description="internal_error",body=Failure),
        (status=503,description="dependency_unavailable or outcome_unknown",body=Failure)))]
async fn create(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(context): State<Context>,
    Json(input): Json<Input>,
) -> Response {
    let input = transfer::Input::new(operation_id, input.message_id, input.code);
    let result = timeout(
        Duration::from_secs(30),
        context.services.transfer().create(actor, input),
    )
    .await;
    match result {
        Ok(Ok(transfer)) => (
            StatusCode::CREATED,
            Json(Receipt {
                code: "ok",
                transfer,
            }),
        )
            .into_response(),
        Ok(Err(error)) => failure(&error),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Failure {
                code: Common::OutcomeUnknown,
            }),
        )
            .into_response(),
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = transfer::CompletionInput)]
struct CompletionInput {
    #[schema(value_type = String, pattern = "^(?:13[0-9]|14[01456879]|15[0-35-9]|16[2567]|17[0-8]|18[0-9]|19[0-35-9])[0-9]{8}$")]
    mobile: Mobile,
    message_id: message::Id,
    #[schema(value_type = String, pattern = "^[0-9]{6}$")]
    code: message::Code,
}

#[derive(Serialize, ToSchema)]
#[schema(as = transfer::Completed)]
struct Completed {
    code: &'static str,
    completion: transfer::completion::Completion,
}

#[utoipa::path(put, path = "/accounts/current/mobile", operation_id = "completeMobileTransfer",
    description = "Replace the current phone after its latest new-phone verification. Completion revokes all existing account sessions. Authenticate with the new phone before reading the transfer status.",
    params(("Operation-Id" = Uuid, Header, description = "Original completion operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Selects the original transfer session cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = CompletionInput, content_type = "application/json"),
    responses((status=200,description="Completed transfer ID and effective time",body=Completed),
        (status=400,description="invalid_input",body=Failure),
        (status=401,description="authentication_invalid",body=Failure),
        (status=403,description="verification_invalid or permission_denied",body=Failure),
        (status=409,description="request_mismatch, state_conflict or mobile_taken",body=Failure),
        (status=410,description="verification_required",body=Failure),
        (status=413,description="invalid_input; body exceeds 8192 bytes",body=Failure),
        (status=415,description="invalid_input",body=Failure),
        (status=422,description="invalid_input",body=Failure),
        (status=429,description="rate_limited",body=Failure,headers(("Retry-After"=u64,description="Wait seconds"))),
        (status=500,description="internal_error",body=Failure),
        (status=503,description="dependency_unavailable or outcome_unknown",body=Failure)))]
async fn complete(
    Extension(actor): Extension<Actor>,
    Extension(operation_id): Extension<operation::Id>,
    State(context): State<Context>,
    Json(input): Json<CompletionInput>,
) -> Response {
    let input =
        transfer::completion::Input::new(operation_id, input.mobile, input.message_id, input.code);
    crate::route::command(
        context.services.transfer().complete(actor, input),
        |completion| {
            Json(Completed {
                code: "ok",
                completion,
            })
            .into_response()
        },
    )
    .await
}

#[utoipa::path(put, path = "/accounts/{id}/mobile", operation_id = "completeAssistedMobileTransfer",
    description = "Complete the manager's original assisted mobile change. Requires current institution.accounts and a strictly lower target. Revoke the target's retained sessions while preserving the manager's session and any target suspension. Exact original retries require current management qualification and do not revoke later sessions.",
    params(("id" = identity::account::Id, Path, description = "Confirmed target account"),
        ("Operation-Id" = Uuid, Header, description = "Original completion operation UUID"),
        ("Session-Id" = Uuid, Header, description = "Selects the original manager session cookie"),
        ("Origin" = String, Header, description = "Required exact configured origin")),
    request_body(content = CompletionInput, content_type = "application/json"),
    responses((status=200,description="Completed transfer ID and effective time",body=Completed),
        (status=400,description="invalid_input",body=Failure),
        (status=401,description="authentication_invalid",body=Failure),
        (status=403,description="verification_invalid or permission_denied",body=Failure),
        (status=404,description="target_unavailable",body=Failure),
        (status=409,description="request_mismatch, state_conflict or mobile_taken",body=Failure),
        (status=410,description="verification_required",body=Failure),
        (status=413,description="invalid_input; body exceeds 8192 bytes",body=Failure),
        (status=415,description="invalid_input",body=Failure),
        (status=422,description="invalid_input",body=Failure),
        (status=429,description="rate_limited",body=Failure,headers(("Retry-After"=u64,description="Wait seconds"))),
        (status=500,description="internal_error",body=Failure),
        (status=503,description="dependency_unavailable or outcome_unknown",body=Failure)))]
async fn complete_assisted(
    Extension(actor): Extension<Actor>,
    Extension(account_id): Extension<identity::account::Id>,
    Extension(operation_id): Extension<operation::Id>,
    State(context): State<Context>,
    Json(input): Json<CompletionInput>,
) -> Response {
    let input =
        transfer::completion::Input::new(operation_id, input.mobile, input.message_id, input.code);
    crate::route::command(
        context
            .services
            .transfer()
            .complete_assisted(actor, account_id, input),
        |completion| {
            Json(Completed {
                code: "ok",
                completion,
            })
            .into_response()
        },
    )
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Serialize, ToSchema)]
#[schema(as = transfer::Status)]
struct Status {
    code: &'static str,
    transfer: transfer::State,
}

#[utoipa::path(get, path = "/mobile-changes/{id}", operation_id = "readMobileTransfer",
    description = "Read a mobile change's minimal status as its person, or as a current manager with institution.accounts and a strictly higher institution level. Paused targets remain readable. Missing and unauthorized targets both return 404. Completed results survive short-term record cleanup. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin.",
    params(("Operation-Id" = Uuid, Header, description = "This read request UUID"),
        ("Session-Id" = Uuid, Header, description = "Selects a currently valid website session cookie"),
        ("id" = transfer::Id, Path, description = "Original transfer UUID")),
    responses((status=200,description="Pending, completed or expired transfer",body=Status),
        (status=400,description="invalid_input; no query or body is accepted",body=Failure),
        (status=401,description="authentication_invalid",body=Failure),
        (status=403,description="permission_denied",body=Failure),
        (status=404,description="target_unavailable",body=Failure),
        (status=413,description="invalid_input",body=Failure),
        (status=429,description="rate_limited",body=Failure,headers(("Retry-After"=u64,description="Wait seconds"))),
        (status=500,description="internal_error",body=Failure),
        (status=503,description="dependency_unavailable",body=Failure)))]
async fn read(
    Extension(request_id): Extension<operation::Id>,
    Extension(actor): Extension<Actor>,
    Extension(id): Extension<transfer::Id>,
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
        context.services.account().fetch_transfer(&actor, id),
    )
    .await
    {
        Ok(Ok(transfer)) => Json(Status {
            code: "ok",
            transfer,
        })
        .into_response(),
        Ok(Err(cause)) => {
            if cause.failure() == account::Failure::Unavailable {
                tracing::error!(operation_id=%request_id, stage="transfer_read", error=?cause);
            }
            let (status, code): (_, Code) = match cause.failure() {
                account::Failure::Invalid => (
                    StatusCode::UNAUTHORIZED,
                    Common::AuthenticationInvalid.into(),
                ),
                account::Failure::Absent | account::Failure::Denied => {
                    (StatusCode::NOT_FOUND, Code::TargetUnavailable)
                }
                account::Failure::Unavailable => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Common::DependencyUnavailable.into(),
                ),
            };
            (status, Json(Failure { code })).into_response()
        }
        Err(cause) => {
            tracing::error!(operation_id=%request_id, stage="transfer_read", error=?cause);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Failure {
                    code: Common::DependencyUnavailable,
                }),
            )
                .into_response()
        }
    }
}

fn failure(error: &transfer::Error) -> Response {
    let (status, code) = match error.failure() {
        transfer::Failure::Invalid => (
            StatusCode::UNAUTHORIZED,
            Common::AuthenticationInvalid.into(),
        ),
        transfer::Failure::Denied => (StatusCode::FORBIDDEN, Common::PermissionDenied.into()),
        transfer::Failure::Absent => (StatusCode::NOT_FOUND, Code::TargetUnavailable),
        transfer::Failure::Mismatch => (StatusCode::CONFLICT, Common::RequestMismatch.into()),
        transfer::Failure::CodeInvalid => (StatusCode::FORBIDDEN, Code::VerificationInvalid),
        transfer::Failure::Verification => (StatusCode::GONE, Code::VerificationRequired),
        transfer::Failure::Conflict => (StatusCode::CONFLICT, Code::StateConflict),
        transfer::Failure::Taken => (StatusCode::CONFLICT, Code::MobileTaken),
        transfer::Failure::Unknown => (
            StatusCode::SERVICE_UNAVAILABLE,
            Common::OutcomeUnknown.into(),
        ),
        transfer::Failure::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Common::DependencyUnavailable.into(),
        ),
    };
    (status, Json(Failure { code })).into_response()
}

#[derive(Clone, Copy, IntoStaticStr, Serialize)]
#[strum(serialize_all = "snake_case")]
#[serde(into = "&'static str")]
enum Code {
    #[strum(transparent)]
    Common(Common),
    VerificationInvalid,
    VerificationRequired,
    StateConflict,
    MobileTaken,
    TargetUnavailable,
    TargetChanged,
}

impl From<Common> for Code {
    fn from(code: Common) -> Self {
        Self::Common(code)
    }
}
impl PartialSchema for Code {
    fn schema() -> RefOr<Schema> {
        Common::schema()
    }
}
impl ToSchema for Code {}
impl response::AsCode for Code {}
