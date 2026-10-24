//! Opaque Turnstile input for an original C1 request.

use std::{
    fmt::{self, Formatter},
    str::FromStr,
};

use derive_more::AsRef;
use thiserror::Error;
use zeroize::Zeroizing;

/// The client-supplied Turnstile proof for an original C1 request.
#[derive(AsRef)]
#[as_ref(str)]
pub struct Token(Zeroizing<String>);

impl Token {
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl FromStr for Token {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.is_empty() {
            return Err(ParseError::Empty);
        }
        if input.chars().count() > 2048 {
            return Err(ParseError::TooLong);
        }

        Ok(Self(Zeroizing::new(input.to_owned())))
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Token").field(&"[redacted]").finish()
    }
}

/// Why a C1 Turnstile token cannot be accepted as input.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("turnstile token must not be empty")]
    Empty,
    #[error("turnstile token exceeds 2048 characters")]
    TooLong,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::identity::challenge::token::{ParseError, Token};

    #[test]
    fn preserves_opaque_token_text() {
        let original = " AbÇ-09\n";
        let token = original.parse::<Token>().expect("fixed token is nonempty");

        assert_eq!(token.as_ref(), original);
        assert_eq!(format!("{token:?}"), "Token(\"[redacted]\")");
    }

    #[test]
    fn accepts_at_most_2048_characters() {
        let boundary = "界".repeat(2048);
        assert_eq!(
            boundary
                .parse::<Token>()
                .expect("2048 characters are allowed")
                .as_ref(),
            boundary
        );

        assert!(matches!("".parse::<Token>(), Err(ParseError::Empty)));
        assert!(matches!(
            "界".repeat(2049).parse::<Token>(),
            Err(ParseError::TooLong)
        ));
    }
}
