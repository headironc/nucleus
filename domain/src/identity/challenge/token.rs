//! Opaque Turnstile input for an original C1 request.

use std::{
    fmt::{self, Formatter},
    str::FromStr,
};

use derive_more::AsRef;
use serde::Deserialize;
use thiserror::Error;
use zeroize::Zeroizing;

/// The client-supplied Turnstile proof for an original C1 request.
#[derive(AsRef, Deserialize)]
#[as_ref(str)]
#[serde(try_from = "Zeroizing<String>")]
pub struct Token(Zeroizing<String>);

impl Token {
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl TryFrom<Zeroizing<String>> for Token {
    type Error = ParseError;

    fn try_from(input: Zeroizing<String>) -> Result<Self, Self::Error> {
        if input.is_empty() {
            return Err(ParseError::Empty);
        }
        if input.chars().count() > 2048 {
            return Err(ParseError::TooLong);
        }

        Ok(Self(input))
    }
}

impl FromStr for Token {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Zeroizing::new(input.to_owned()).try_into()
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

    #[test]
    fn deserialization_preserves_opaque_text_and_rejects_invalid_values() {
        for original in [" AbÇ-09\n".to_owned(), "界".repeat(2048)] {
            let json = serde_json::to_string(&original).expect("JSON text");
            let token: Token = serde_json::from_str(&json).expect("valid token text");
            assert_eq!(token.as_ref(), original);
            let token: Token = serde_json::from_value(serde_json::json!(original))
                .expect("valid owned token text");
            assert_eq!(token.as_ref(), original);
        }
        let token: Token = serde_json::from_str(r#""\u0061""#).expect("escaped opaque text");
        assert_eq!(token.as_ref(), "a");

        let private = "界".repeat(2049);
        for value in [serde_json::json!(""), serde_json::json!(private)] {
            let json = value.to_string();
            for error in [
                serde_json::from_str::<Token>(&json).expect_err("invalid token text"),
                serde_json::from_value::<Token>(value).expect_err("invalid owned token text"),
            ] {
                assert_eq!(error.classify(), serde_json::error::Category::Data);
                assert!(!format!("{error:?}").contains(&private));
                assert!(!error.to_string().contains(&private));
            }
        }
        for json in ["null", "123", "true", "[]", "{}"] {
            assert!(serde_json::from_str::<Token>(json).is_err());
        }
    }
}
