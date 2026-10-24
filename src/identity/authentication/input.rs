//! The C1 JSON body and its conversion through domain value constructors.

use std::{fmt, net::IpAddr};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use domain::identity::{authentication, challenge, mobile, operation};
use serde::Deserialize;
use thiserror::Error;
use utoipa::ToSchema;
use zeroize::Zeroizing;

use crate::{
    json::Json,
    response::{Common, Failure},
};

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Input {
    #[schema(value_type = String, pattern = "^(?:13[0-9]|14[01456879]|15[0-35-9]|16[2567]|17[0-8]|18[0-9]|19[0-35-9])[0-9]{8}$")]
    mobile: Zeroizing<String>,
    #[schema(value_type = String, min_length = 1, max_length = 2048)]
    token: Zeroizing<String>,
}

impl Input {
    pub(super) fn parse(
        self,
        operation_id: operation::Id,
        source: IpAddr,
    ) -> Result<authentication::Input, Error> {
        Ok(authentication::Input::new(
            operation_id,
            self.mobile.parse()?,
            self.token.parse()?,
            source,
        ))
    }
}

#[derive(Error)]
pub(super) enum Error {
    #[error("invalid C1 mobile number")]
    Mobile(#[from] mobile::ParseError),
    #[error("invalid C1 token")]
    Token(#[from] challenge::token::ParseError),
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(Failure {
                code: Common::InvalidInput,
            }),
        )
            .into_response()
    }
}
