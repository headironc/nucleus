//! The R1 credential for a future registration; it does not create an account.

pub(crate) mod attempt;

use std::str::FromStr;

use derive_more::Display;
use serde::{Deserialize, Serialize};
use serde_with::DeserializeFromStr;
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::identity::credential;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Display,
    Serialize,
    DeserializeFromStr,
    FromSql,
    ToSql,
    ToSchema,
)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = registration::Id)]
pub struct Id(Uuid);
impl Id {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl FromStr for Id {
    type Err = ParseIdError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(input)?))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid UUID text")]
pub struct ParseIdError(#[from] uuid::Error);

#[derive(Debug, Deserialize)]
#[serde(try_from = "Zeroizing<String>")]
pub struct Token(pub(crate) credential::Secret);
impl Token {
    /// Explicitly expose the original registration token at its JSON delivery boundary.
    pub fn text(&self) -> Zeroizing<String> {
        self.0.text()
    }
}

impl FromStr for Token {
    type Err = credential::ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(input.parse()?))
    }
}

impl TryFrom<Zeroizing<String>> for Token {
    type Error = credential::ParseError;

    fn try_from(input: Zeroizing<String>) -> Result<Self, Self::Error> {
        input.parse()
    }
}
