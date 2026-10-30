//! The server-selected application identity for mini-program phone authentication.

pub mod client;

use std::str::FromStr;

use serde::Deserialize;
use zeroize::Zeroizing;

use crate::identity::website;

#[derive(Deserialize)]
#[serde(try_from = "Zeroizing<String>")]
pub struct Code(Zeroizing<String>);

impl TryFrom<Zeroizing<String>> for Code {
    type Error = CodeError;

    fn try_from(value: Zeroizing<String>) -> Result<Self, Self::Error> {
        if value.is_empty() || value.chars().count() > 2048 {
            return Err(CodeError);
        }
        Ok(Self(value))
    }
}

impl FromStr for Code {
    type Err = CodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Zeroizing::new(value.to_owned()).try_into()
    }
}

impl Code {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("phone authorization code must contain 1 to 2048 characters")]
pub struct CodeError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppId(website::AppId);

impl FromStr for AppId {
    type Err = website::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.parse()?))
    }
}

impl AsRef<str> for AppId {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}
