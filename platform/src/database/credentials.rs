//! Validated database login password.

use std::fmt;

use derive_more::AsRef;
use serde::Deserialize;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::credentials::deserialize_version;

/// File shape only; password validation requires explicit conversion.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    #[serde(deserialize_with = "deserialize_version")]
    version: (),
    password: Zeroizing<String>,
}

#[derive(AsRef)]
#[as_ref([u8])]
pub struct Password(Zeroizing<String>);

impl Password {
    pub fn new(input: String) -> Result<Self, Error> {
        let input = Zeroizing::new(input);
        if input.is_empty() || input.len() > 4096 || input.chars().any(char::is_control) {
            return Err(Error);
        }
        Ok(Self(input))
    }
}

impl TryFrom<Input> for Password {
    type Error = Error;

    fn try_from(
        Input {
            version: (),
            mut password,
        }: Input,
    ) -> Result<Self, Error> {
        Self::new(std::mem::take(&mut *password))
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Password")
            .field(&"[redacted]")
            .finish()
    }
}

#[derive(Debug, Error)]
#[error("invalid database password")]
pub struct Error;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::database::credentials::{Input, Password};

    #[test]
    fn parses_password_text_before_explicit_validation() {
        let input: Input =
            serde_json::from_str(r#"{"version":1,"password":""}"#).expect("valid input shape");
        assert!(Password::try_from(input).is_err());
        let input: Input =
            serde_json::from_str(r#"{"version":1,"password":" fictional password "}"#)
                .expect("valid input shape");
        let password: Password = input.try_into().expect("valid password");
        assert_eq!(password.as_ref(), b" fictional password ");
    }

    #[test]
    fn preserves_text_and_enforces_byte_boundaries() {
        for input in [" fictional password ".to_owned(), "é".repeat(2048)] {
            let password = Password::new(input.clone()).expect("valid password");
            assert_eq!(password.as_ref(), input.as_bytes());
            assert_eq!(format!("{password:?}"), "Password(\"[redacted]\")");
        }
        for input in [
            String::new(),
            "secret\n".into(),
            "secret\0".into(),
            "x".repeat(4097),
        ] {
            assert!(Password::new(input).is_err());
        }
    }
}
