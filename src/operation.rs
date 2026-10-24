//! Request operation identifiers use the domain's value parser.

use std::fmt;

use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use domain::identity::operation;
use thiserror::Error;

use crate::{json::Json, response};

pub(crate) async fn require(mut request: Request, next: Next) -> Result<Response, Error> {
    let operation_id = parse(request.headers())?;
    request.extensions_mut().insert(operation_id);
    tracing::Span::current().record("operation_id", tracing::field::display(operation_id));
    Ok(next.run(request).await)
}

fn parse(headers: &HeaderMap) -> Result<operation::Id, Error> {
    let mut values = headers.get_all("operation-id").iter();
    let value = values.next().ok_or(Error::Missing)?;
    if values.next().is_some() {
        return Err(Error::Repeated);
    }
    Ok(value.to_str()?.parse()?)
}

#[derive(Error)]
pub(crate) enum Error {
    #[error("operation identifier is missing")]
    Missing,
    #[error("operation identifier is repeated")]
    Repeated,
    #[error("invalid operation identifier header")]
    Header(#[from] axum::http::header::ToStrError),
    #[error("invalid operation identifier UUID")]
    Value(#[from] operation::ParseError),
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        tracing::info!(stage = "operation_input", kind = "invalid_input", error = ?self);
        (
            StatusCode::BAD_REQUEST,
            Json(response::Failure {
                code: response::Common::InvalidInput,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use axum::{
        body::to_bytes,
        http::{HeaderMap, HeaderValue, StatusCode},
        response::IntoResponse,
    };

    use crate::operation::{Error, parse};

    #[tokio::test]
    async fn retains_header_failures_without_echoing_unverified_identifiers() {
        let mut headers = HeaderMap::new();
        let missing = match parse(&headers) {
            Ok(_) => panic!("missing identifier"),
            Err(error) => error,
        };
        assert!(matches!(missing, Error::Missing));

        headers.insert("operation-id", HeaderValue::from_static("private-marker"));
        let invalid = match parse(&headers) {
            Ok(_) => panic!("invalid identifier"),
            Err(error) => error,
        };
        assert!(matches!(invalid, Error::Value(_)));
        assert!(std::error::Error::source(&invalid).is_some());
        assert!(!format!("{invalid:?} {invalid}").contains("private-marker"));

        headers.insert(
            "operation-id",
            HeaderValue::from_bytes(&[0xff]).expect("header bytes"),
        );
        let encoding = match parse(&headers) {
            Ok(_) => panic!("invalid header text"),
            Err(error) => error,
        };
        assert!(matches!(encoding, Error::Header(_)));
        assert!(std::error::Error::source(&encoding).is_some());

        headers.insert(
            "operation-id",
            HeaderValue::from_static("550e8400-e29b-41d4-a716-446655440000"),
        );
        headers.append(
            "operation-id",
            HeaderValue::from_static("550e8400-e29b-41d4-a716-446655440000"),
        );
        let repeated = match parse(&headers) {
            Ok(_) => panic!("repeated identifier"),
            Err(error) => error,
        };
        assert!(matches!(repeated, Error::Repeated));

        for error in [missing, invalid, encoding, repeated] {
            let response = error.into_response();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let bytes = to_bytes(response.into_body(), 1024)
                .await
                .expect("error body");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).expect("error JSON"),
                serde_json::json!({"code":"invalid_input"})
            );
        }
    }
}
