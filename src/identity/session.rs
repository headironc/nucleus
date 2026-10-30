//! Website session projection, selected cookie parsing, delivery and expiry.

use std::fmt;

use axum::http::{HeaderMap, HeaderValue, header};
use chrono::{DateTime, TimeDelta, Timelike, Utc};
use cookie::{Cookie, SameSite, time::OffsetDateTime};
use domain::identity::session;
use serde::Serialize;
use utoipa::ToSchema;
use zeroize::Zeroizing;

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = session::Session, max_properties = 2)]
pub(crate) struct Session {
    pub(crate) id: session::Id,
    pub(crate) expires_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
#[serde(untagged)]
#[schema(as = session::Delivery)]
pub(crate) enum Delivery {
    Website(Session),
    Mini(Mini),
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = session::Mini, max_properties = 3)]
pub(crate) struct Mini {
    pub(crate) id: session::Id,
    #[schema(value_type = String, min_length = 43, max_length = 43, pattern = "^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$")]
    pub(crate) token: Zeroizing<String>,
    pub(crate) expires_at: DateTime<Utc>,
}

fn name(id: session::Id) -> String {
    format!("__Host-nucleus-session-{id}")
}

pub(super) fn present(headers: &HeaderMap) -> Result<bool, ParseError> {
    for value in headers.get_all(header::COOKIE) {
        for cookie in Cookie::split_parse(value.to_str()?) {
            if cookie?.name().starts_with("__Host-nucleus-session-") {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub(super) fn read(headers: &HeaderMap, id: session::Id) -> Result<session::Token, ParseError> {
    let name = name(id);
    let mut option = None;
    for value in headers.get_all(header::COOKIE) {
        for cookie in Cookie::split_parse(value.to_str()?) {
            let cookie = cookie?;
            if cookie.name() != name {
                continue;
            }
            if option.is_some() {
                return Err(ParseError::Repeated);
            }
            option = Some(cookie.value().parse()?);
        }
    }
    option.ok_or(ParseError::Missing)
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ParseError {
    #[error("selected session cookie is absent")]
    Missing,
    #[error("selected session cookie is repeated")]
    Repeated,
    #[error("cookie header is malformed")]
    Header(#[from] header::ToStrError),
    #[error("cookie structure is malformed")]
    Cookie(#[from] cookie::ParseError),
    #[error("{0}")]
    Token(#[from] session::ParseError),
}

pub(crate) fn cookie(
    id: session::Id,
    token: &session::Token,
    expires_at: DateTime<Utc>,
) -> Result<HeaderValue, Error> {
    let at = if expires_at.nanosecond() == 0 {
        expires_at
    } else {
        expires_at
            .checked_add_signed(TimeDelta::nanoseconds(i64::from(
                1_000_000_000 - expires_at.nanosecond(),
            )))
            .ok_or(Error::Expiry)?
    };
    let at = OffsetDateTime::from_unix_timestamp(at.timestamp())?;
    let token = token.text();
    let cookie = Cookie::build((name(id), token.as_str()))
        .secure(true)
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .expires(at)
        .build();
    let text = Zeroizing::new(cookie.to_string());
    let mut value = HeaderValue::from_str(&text)?;
    value.set_sensitive(true);
    Ok(value)
}

pub(crate) fn clear(id: session::Id) -> Result<HeaderValue, Error> {
    let cookie = Cookie::build((name(id), ""))
        .secure(true)
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .expires(OffsetDateTime::UNIX_EPOCH)
        .build();
    let mut value = HeaderValue::from_str(&cookie.to_string())?;
    value.set_sensitive(true);
    Ok(value)
}

#[derive(thiserror::Error)]
pub(crate) enum Error {
    #[error("original session expiry cannot be rounded for its cookie")]
    Expiry,
    #[error("original session expiry is outside the cookie date range")]
    Date(#[from] cookie::time::error::ComponentRange),
    #[error("original session cookie cannot be encoded as a response header")]
    Header(#[from] header::InvalidHeaderValue),
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}
