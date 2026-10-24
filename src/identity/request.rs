//! Anonymous admission and shared ingress failure responses precede business work.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use thiserror::Error;

use crate::{
    identity::{Context, admission},
    json::Json,
    response, source,
};

pub(super) async fn admit(
    State(context): State<Context>,
    source: source::Source,
    mut request: Request,
    next: Next,
) -> Result<Response, Error> {
    check(&context, &source, &request, false)?;
    request.extensions_mut().insert(source);
    Ok(next.run(request).await)
}

pub(super) async fn admit_session(
    State(context): State<Context>,
    source: source::Source,
    mut request: Request,
    next: Next,
) -> Result<Response, Error> {
    check(&context, &source, &request, true)?;
    request.extensions_mut().insert(source);
    Ok(next.run(request).await)
}

fn check(
    context: &Context,
    source: &source::Source,
    request: &Request,
    create_session: bool,
) -> Result<(), Error> {
    if create_session {
        context.admission.create_session(source.address())?;
    } else {
        context.admission.accept(source.address())?;
    }
    let mut origins = request.headers().get_all("origin").iter();
    let origin = origins.next().ok_or(Error::Origin)?;
    if origins.next().is_some()
        || origin.as_bytes() != context.settings.origin().ascii_serialization().as_bytes()
    {
        return Err(Error::Origin);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub(super) enum Error {
    #[error("identity ingress admission failed")]
    Admission(#[from] admission::Error),
    #[error("website identity origin is missing or not allowed")]
    Origin,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::Admission(admission::Error::Limited(wait)) => return response::limited(wait),
            Self::Admission(cause) => {
                tracing::error!(stage = "http_admission", kind = "unavailable", error = ?cause);
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    response::Common::DependencyUnavailable,
                )
            }
            Self::Origin => (StatusCode::FORBIDDEN, response::Common::PermissionDenied),
        };
        (status, Json(response::Failure { code })).into_response()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use axum::{body::to_bytes, http::StatusCode, response::IntoResponse};

    use crate::identity::{admission, request::Error};

    #[tokio::test]
    async fn unavailable_budget_state_has_one_diagnostic_and_a_public_failure_code() {
        let output = tempfile::NamedTempFile::new().expect("diagnostic capture");
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_writer(output.reopen().expect("capture writer"))
            .finish();
        let response = tracing::subscriber::with_default(subscriber, || {
            Error::Admission(admission::Error::State).into_response()
        });
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!response.headers().contains_key("retry-after"));
        let body = to_bytes(response.into_body(), 1024)
            .await
            .expect("public failure body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).expect("failure JSON"),
            serde_json::json!({"code":"dependency_unavailable"}),
        );
        let text = std::fs::read_to_string(output.path()).expect("captured event");
        let events: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON diagnostic"))
            .collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["level"], "ERROR");
        assert_eq!(events[0]["fields"]["stage"], "http_admission");
        assert_eq!(events[0]["fields"]["kind"], "unavailable");
        assert_eq!(events[0]["fields"]["error"], "State");
    }
}
