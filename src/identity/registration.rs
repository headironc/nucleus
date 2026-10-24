//! Registration HTTP input and original website session delivery.

use std::time::Duration;

use axum::{
    extract::State,
    http::{StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
};
use domain::{
    identity::{
        operation,
        person::{name::Name, number::IdNumber},
        registration,
    },
    registration::Failure,
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
    identity::{Context, request, session},
    json::Json,
    response,
};

pub(super) fn router(context: &Context) -> OpenApiRouter<Context> {
    OpenApiRouter::new().routes(routes!(create).map(|router| {
        router.route_layer(middleware::from_fn_with_state(
            context.clone(),
            request::admit_session,
        ))
    }))
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = registration::Input)]
struct Input {
    id: registration::Id,
    #[schema(value_type = String, min_length = 43, max_length = 43)]
    token: registration::Token,
    name: Name,
    #[schema(value_type = String)]
    id_number: IdNumber,
}

#[derive(Serialize, ToSchema)]
#[schema(as = registration::Receipt)]
struct Receipt {
    code: &'static str,
    session: session::Session,
}

#[utoipa::path(post, path = "/registrations", operation_id = "register",
    params(("Operation-Id" = Uuid, Header, description = "Original R2 operation UUID"),
        ("Origin" = String, Header, description = "Required configured HTTPS origin")),
    request_body(content = Input, content_type = "application/json"),
    responses(
        (status = 201, description = "Account created; original session cookie", body = Receipt, headers(("Set-Cookie" = String, description = "Original secure session cookie"))),
        (status = 200, description = "Original successful registration session", body = Receipt, headers(("Set-Cookie" = String, description = "The same original secure session cookie"))),
        (status = 400, description = "invalid_input", body = response::Failure),
        (status = 401, description = "authentication_invalid", body = response::Failure),
        (status = 403, description = "permission_denied", body = response::Failure),
        (status = 409, description = "request_mismatch, mobile_taken or id_number_taken", body = response::Failure),
        (status = 410, description = "verification_required", body = response::Failure),
        (status = 413, description = "invalid_input", body = response::Failure),
        (status = 415, description = "invalid_input", body = response::Failure),
        (status = 422, description = "invalid_input", body = response::Failure),
        (status = 429, description = "rate_limited", body = response::Failure, headers(("Retry-After" = u64, description = "Whole seconds, rounded up, at least one"))),
        (status = 500, description = "internal_error", body = response::Failure),
        (status = 503, description = "dependency_unavailable or outcome_unknown", body = response::Failure)
    )
)]
async fn create(
    Extension(operation_id): Extension<operation::Id>,
    State(context): State<Context>,
    Json(input): Json<Input>,
) -> Response {
    let input = domain::registration::Input::new(
        operation_id,
        input.id,
        input.token,
        input.name,
        input.id_number,
    );
    let receipt = match timeout(
        Duration::from_secs(30),
        context.services.registration().register(input),
    )
    .await
    {
        Ok(Ok(receipt)) => receipt,
        Ok(Err(cause)) => return failure(cause, &context, operation_id).await,
        Err(_) => {
            return failed(
                StatusCode::SERVICE_UNAVAILABLE,
                response::Common::OutcomeUnknown.into(),
            );
        }
    };
    let cookie = match session::cookie(receipt.id(), receipt.token(), receipt.expires_at()) {
        Ok(cookie) => cookie,
        Err(cause) => {
            tracing::error!(%operation_id, stage = "registration_cookie", kind = "internal", error = ?cause);
            return failed(
                StatusCode::INTERNAL_SERVER_ERROR,
                response::Common::InternalError.into(),
            );
        }
    };
    (
        if receipt.created() {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        [(header::SET_COOKIE, cookie)],
        Json(Receipt {
            code: "ok",
            session: session::Session {
                id: receipt.id(),
                expires_at: receipt.expires_at(),
            },
        }),
    )
        .into_response()
}

async fn failure(
    cause: domain::registration::Error,
    context: &Context,
    operation_id: operation::Id,
) -> Response {
    use response::Common;
    let (status, code) = match cause.failure() {
        Failure::Proof => (
            StatusCode::UNAUTHORIZED,
            Common::AuthenticationInvalid.into(),
        ),
        Failure::Mismatch => (StatusCode::CONFLICT, Common::RequestMismatch.into()),
        Failure::Verification => (StatusCode::GONE, Code::VerificationRequired),
        Failure::InvalidDate => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Common::InvalidInput.into(),
        ),
        Failure::MobileTaken => (StatusCode::CONFLICT, Code::MobileTaken),
        Failure::IdNumberTaken => (StatusCode::CONFLICT, Code::IdNumberTaken),
        Failure::Quota { retry_at } => match context.services.retry_after(retry_at).await {
            Ok(wait) => return response::limited(wait),
            Err(error) => {
                tracing::error!(%operation_id, stage = "registration_quota_response", kind = "unavailable", quota = ?cause, error = ?error);
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Common::DependencyUnavailable.into(),
                )
            }
        },
        Failure::Unknown => (
            StatusCode::SERVICE_UNAVAILABLE,
            Common::OutcomeUnknown.into(),
        ),
        Failure::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            Common::DependencyUnavailable.into(),
        ),
    };
    failed(status, code)
}

fn failed(status: StatusCode, code: Code) -> Response {
    (status, Json(response::Failure { code })).into_response()
}

#[derive(Debug, Clone, Copy, IntoStaticStr, Serialize)]
#[strum(serialize_all = "snake_case")]
#[serde(into = "&'static str")]
enum Code {
    #[strum(transparent)]
    Common(response::Common),
    VerificationRequired,
    MobileTaken,
    IdNumberTaken,
}

impl From<response::Common> for Code {
    fn from(value: response::Common) -> Self {
        Self::Common(value)
    }
}
impl response::AsCode for Code {}
impl PartialSchema for Code {
    fn schema() -> RefOr<Schema> {
        response::Common::schema()
    }
}
impl ToSchema for Code {}
