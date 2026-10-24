//! C1 request admission precedes body reads and business work.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use thiserror::Error;

use crate::{
    identity::{admission, authentication::Context},
    json::Json,
    response, source,
};

pub(super) async fn admit(
    State(context): State<Context>,
    source: source::Source,
    mut request: Request,
    next: Next,
) -> Result<Response, Error> {
    if let Err(cause) = context.admission.accept(source.address()) {
        if !matches!(cause, admission::Error::Limited(_)) {
            tracing::error!(stage = "http_admission", kind = "unavailable", error = ?cause);
        }
        return Err(cause.into());
    }
    let mut origins = request.headers().get_all("origin").iter();
    let origin = origins.next().ok_or(Error::Origin)?;
    if origins.next().is_some()
        || origin.as_bytes() != context.settings.origin().ascii_serialization().as_bytes()
    {
        return Err(Error::Origin);
    }
    request.extensions_mut().insert(source);
    Ok(next.run(request).await)
}

#[derive(Debug, Error)]
pub(super) enum Error {
    #[error("C1 ingress admission failed")]
    Admission(#[from] admission::Error),
    #[error("C1 request origin is missing or not allowed")]
    Origin,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::Admission(admission::Error::Limited(wait)) => return response::limited(wait),
            Self::Admission(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                response::Common::DependencyUnavailable,
            ),
            Self::Origin => (StatusCode::FORBIDDEN, response::Common::PermissionDenied),
        };
        (status, Json(response::Failure { code })).into_response()
    }
}
