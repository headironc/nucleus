//! Credential selection and decisive self-account projection share one website boundary.

use std::time::Duration;

use axum::{
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use domain::identity::{
    account,
    current::{self, Actor, Proof},
    person::{self, name::Name},
    session, wechat,
};
use serde::Serialize;
use tokio::time::{Instant, timeout, timeout_at};
use utoipa::ToSchema;
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouterExt},
    routes,
};

use crate::{
    extension::Extension,
    identity::{Context, admission, avatar, person::IdNumber, request},
    json::Json,
    response::{Common, Failure},
    source,
};

pub(super) fn router(context: &Context) -> OpenApiRouter<Context> {
    OpenApiRouter::new().routes(routes!(read).map(|router| {
        router.route_layer(middleware::from_fn_with_state(
            context.clone(),
            authenticate,
        ))
    }))
}

/// The absolute upload admission deadline shared by authentication, body and business work.
#[derive(Clone, Copy)]
pub(crate) struct Deadline(pub(crate) Instant);

pub(crate) const UPLOAD_TOTAL: Duration = Duration::from_secs(30);

pub(super) async fn authenticate_upload(
    State(context): State<Context>,
    source: source::Source,
    mut request: Request,
    next: Next,
) -> Result<Response, Error> {
    let deadline = Instant::now() + UPLOAD_TOTAL;
    context.admission.write(source.address())?;
    authenticate_at(context, &mut request, deadline, true).await?;
    request.extensions_mut().insert(Deadline(deadline));
    Ok(timeout_at(deadline, next.run(request)).await?)
}

pub(super) async fn authenticate_original(
    State(context): State<Context>,
    source: source::Source,
    mut request: Request,
    next: Next,
) -> Result<Response, Error> {
    let deadline = Instant::now() + UPLOAD_TOTAL;
    context.admission.read(source.address())?;
    authenticate_at(context, &mut request, deadline, false).await?;
    request.extensions_mut().insert(Deadline(deadline));
    Ok(timeout_at(deadline, next.run(request)).await?)
}

async fn authenticate_at(
    context: Context,
    request: &mut Request,
    deadline: Instant,
    writing: bool,
) -> Result<(), Error> {
    let proof = proof(request.headers())?;
    if writing && !request.headers().contains_key("origin") {
        return Err(Error::Origin);
    }
    origin(request.headers(), &context)?;
    let actor = timeout_at(deadline, context.services.current().authenticate(proof)).await??;
    request.extensions_mut().insert(actor);
    Ok(())
}

pub(crate) async fn authenticate(
    State(context): State<Context>,
    source: source::Source,
    request: Request,
    next: Next,
) -> Result<Response, impl IntoResponse> {
    let writing = matches!(
        *request.method(),
        Method::DELETE | Method::POST | Method::PUT
    );
    if writing {
        context.admission.write(source.address())?;
    } else {
        context.admission.read(source.address())?;
    }
    authenticate_request(context, request, next).await
}

pub(crate) async fn authenticate_code(
    State(context): State<Context>,
    source: source::Source,
    mut request: Request,
    next: Next,
) -> Result<Response, impl IntoResponse> {
    context.admission.accept(source.address())?;
    request.extensions_mut().insert(source);
    authenticate_request(context, request, next).await
}

async fn authenticate_request(
    context: Context,
    mut request: Request,
    next: Next,
) -> Result<Response, Error> {
    let writing = matches!(
        *request.method(),
        Method::DELETE | Method::POST | Method::PUT
    );
    let proof = proof(request.headers())?;
    if writing && !request.headers().contains_key("origin") {
        return Err(Error::Origin);
    }
    origin(request.headers(), &context)?;
    let actor = timeout(
        Duration::from_secs(30),
        context.services.current().authenticate(proof),
    )
    .await??;
    request.extensions_mut().insert(actor);
    Ok(next.run(request).await)
}

fn proof(headers: &HeaderMap) -> Result<Proof, Error> {
    if headers.contains_key("authorization") {
        return Err(Error::Input);
    }
    let mut values = headers.get_all("session-id").iter();
    let value = values.next().ok_or(Error::Missing)?;
    if values.next().is_some() {
        return Err(Error::Input);
    }
    let id: session::Id = value.to_str()?.parse()?;
    Ok(Proof {
        id,
        token: crate::identity::session::read(headers, id)?,
    })
}

fn origin(headers: &HeaderMap, context: &Context) -> Result<(), Error> {
    let mut origins = headers.get_all("origin").iter();
    let mut sites = headers.get_all("sec-fetch-site").iter();
    let supplied = match origins.next() {
        Some(value)
            if value.as_bytes() == context.settings.origin().ascii_serialization().as_bytes() =>
        {
            true
        }
        Some(_) => return Err(Error::Origin),
        None => false,
    };
    match sites.next() {
        Some(value) if value.as_bytes() == b"same-origin" => {}
        None if supplied => {}
        Some(_) | None => return Err(Error::Origin),
    }
    if origins.next().is_some() || sites.next().is_some() {
        return Err(Error::Origin);
    }
    Ok(())
}

#[derive(Serialize, ToSchema)]
#[schema(as = current::Receipt, max_properties = 2)]
struct Receipt<'a> {
    code: &'static str,
    account: Account<'a>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = current::Account, max_properties = 7)]
struct Account<'a> {
    id: account::Id,
    person_id: person::Id,
    revision_id: person::revision::Id,
    #[schema(value_type = String, min_length = 1, max_length = 64, pattern = r#"^[^\p{Cc}\p{Cs}\p{White_Space}](?:[^\p{Cc}\p{Cs}]*[^\p{Cc}\p{Cs}\p{White_Space}])?(?![\s\S])"#)]
    name: &'a Name,
    id_number: IdNumber<'a>,
    avatar: avatar::Current<'a>,
    #[schema(required)]
    binding: Option<Binding>,
}
#[derive(Serialize, ToSchema)]
#[schema(as = current::Binding, max_properties = 1)]
struct Binding {
    revision: wechat::Revision,
}

#[utoipa::path(get, path = "/accounts/current", operation_id = "currentAccount",
    description = "Website same-origin read. Origin must match the configured origin when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin.",
    params(("Operation-Id" = Uuid, Header, description = "Required request UUID; no persistent read operation"),
        ("Session-Id" = Uuid, Header, description = "Required website session UUID; selects its canonical cookie name; Authorization is rejected")),
    responses((status = 200, description = "Complete current account; no session refresh", body = Receipt),
        (status = 400, description = "invalid_input", body = Failure), (status = 401, description = "authentication_invalid", body = Failure),
        (status = 403, description = "permission_denied", body = Failure), (status = 429, description = "rate_limited", body = Failure, headers(("Retry-After" = u64, description = "Wait seconds before another attempt"))),
        (status = 503, description = "dependency_unavailable", body = Failure)))]
async fn read(
    Extension(actor): Extension<Actor>,
    State(context): State<Context>,
) -> Result<Response, Error> {
    let account = timeout(
        Duration::from_secs(30),
        context.services.current().read(&actor),
    )
    .await??;
    let avatar = account.avatar();
    Ok(Json(Receipt {
        code: "ok",
        account: Account {
            id: account.id(),
            person_id: account.person_id(),
            revision_id: account.revision_id(),
            name: account.name(),
            id_number: IdNumber {
                suffix: account.suffix(),
            },
            avatar: avatar.into(),
            binding: None,
        },
    })
    .into_response())
}

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("session selection is malformed or ambiguous")]
    Input,
    #[error("session proof is absent")]
    Missing,
    #[error("website request origin is not allowed")]
    Origin,
    #[error("{0}")]
    Id(#[from] session::ParseIdError),
    #[error("session header is malformed")]
    Header(#[from] axum::http::header::ToStrError),
    #[error("{0}")]
    Cookie(#[from] crate::identity::session::ParseError),
    #[error("{0}")]
    Admission(#[from] admission::Error),
    #[error("{0}")]
    Current(#[from] current::Error),
    #[error("current account request budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::Missing
            | Self::Cookie(crate::identity::session::ParseError::Missing)
            | Self::Current(current::Error::Invalid) => {
                (StatusCode::UNAUTHORIZED, Common::AuthenticationInvalid)
            }
            Self::Input | Self::Id(_) | Self::Header(_) | Self::Cookie(_) => {
                (StatusCode::BAD_REQUEST, Common::InvalidInput)
            }
            Self::Origin => (StatusCode::FORBIDDEN, Common::PermissionDenied),
            Self::Admission(cause) => return request::Error::Admission(cause).into_response(),
            cause => {
                tracing::error!(stage="current_account", kind="unavailable", error=?cause);
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Common::DependencyUnavailable,
                )
            }
        };
        (status, Json(Failure { code })).into_response()
    }
}
