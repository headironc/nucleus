//! The Siteverify server credential and its validation.

use std::{
    fmt::{self, Formatter},
    str::FromStr,
};

use derive_more::AsRef;
use thiserror::Error;
use zeroize::Zeroizing;

/// The Siteverify server secret supplied by application assembly.
#[derive(AsRef)]
#[as_ref(str)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(input: String) -> Result<Self, ParseError> {
        let input = Zeroizing::new(input);
        if input.is_empty() {
            return Err(ParseError::Empty);
        }
        if input
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(ParseError::InvalidCharacter);
        }

        Ok(Self(input))
    }
}

impl FromStr for Secret {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::new(input.to_owned())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Secret")
            .field(&"[redacted]")
            .finish()
    }
}

/// Why a Siteverify server secret is rejected locally.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("siteverify secret is empty")]
    Empty,
    #[error("siteverify secret contains whitespace or a control character")]
    InvalidCharacter,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::identity::challenge::credentials::{ParseError, Secret};
    fn server_secret() -> Secret {
        Secret::new("test-secret".into()).expect("fixed server secret is valid")
    }

    #[test]
    fn validates_and_hides_siteverify_server_secret() {
        let secret = server_secret();
        assert_eq!(secret.as_ref(), "test-secret");
        assert_eq!(format!("{secret:?}"), "Secret(\"[redacted]\")");
        assert_eq!(
            "test-secret"
                .parse::<Secret>()
                .expect("text parser")
                .as_ref(),
            secret.as_ref()
        );
        assert!(matches!(Secret::new(String::new()), Err(ParseError::Empty)));
        assert!(matches!("".parse::<Secret>(), Err(ParseError::Empty)));
        for input in ["bad secret", "bad\nsecret", "bad\0secret"] {
            assert!(matches!(
                Secret::new(input.into()),
                Err(ParseError::InvalidCharacter)
            ));
            assert!(matches!(
                input.parse::<Secret>(),
                Err(ParseError::InvalidCharacter)
            ));
        }
    }
}
