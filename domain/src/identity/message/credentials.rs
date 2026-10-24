//! Validated in-memory RAM credentials for the message supplier.

use std::fmt::{self, Debug, Formatter};

use derive_more::AsRef;
use thiserror::Error;
use zeroize::Zeroizing;

/// An AccessKey ID accepted by the local PNVS credential contract.
#[derive(AsRef)]
#[as_ref(str)]
pub struct Id(Zeroizing<String>);

impl Id {
    /// Preserve a nonempty ASCII alphanumeric ID without normalization.
    pub fn new(input: String) -> Result<Self, IdError> {
        let input = Zeroizing::new(input);
        if input.is_empty() || !input.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(IdError);
        }
        Ok(Self(input))
    }
}

impl Debug for Id {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Id").field(&"[redacted]").finish()
    }
}

/// An AccessKey ID failed the local contract, without echoing its value.
#[derive(Debug, Error)]
#[error("invalid PNVS access key id")]
pub struct IdError;

/// An AccessKey Secret distinct from other supplier and client secrets.
#[derive(AsRef)]
#[as_ref(str)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Preserve a nonempty secret without whitespace or control characters.
    pub fn new(input: String) -> Result<Self, SecretError> {
        let input = Zeroizing::new(input);
        if input.is_empty()
            || input
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(SecretError);
        }
        Ok(Self(input))
    }
}

impl Debug for Secret {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Secret")
            .field(&"[redacted]")
            .finish()
    }
}

/// An AccessKey Secret failed the local contract, without echoing its value.
#[derive(Debug, Error)]
#[error("invalid PNVS access key secret")]
pub struct SecretError;

/// A pair of validated values; construction does not verify supplier authority.
#[derive(Debug)]
pub struct AccessKey {
    id: Id,
    secret: Secret,
}

impl AccessKey {
    /// Pair already validated credential values for application assembly.
    pub fn new(id: Id, secret: Secret) -> Self {
        Self { id, secret }
    }

    pub(super) fn id(&self) -> &Id {
        &self.id
    }

    pub(super) fn secret(&self) -> &Secret {
        &self.secret
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::identity::message::credentials::{AccessKey, Id, Secret};

    #[test]
    fn access_key_id_preserves_text_and_rejects_invalid_supplied_values() {
        let id = Id::new("ExampleId123".into()).expect("ASCII alphanumeric ID is valid");
        assert_eq!(id.as_ref(), "ExampleId123");
        assert_eq!(format!("{id:?}"), "Id(\"[redacted]\")");
        for input in [
            "",
            "id,Signature=injected",
            "id_",
            "id=",
            "id.secret",
            "密钥",
            "id\n",
            "id ",
        ] {
            let error = Id::new(input.into()).expect_err("invalid ID fails");
            assert_eq!(
                format!("{error:?}: {error}"),
                "IdError: invalid PNVS access key id"
            );
        }
    }

    #[test]
    fn access_key_secret_preserves_text_and_rejects_invalid_supplied_values() {
        let secret = Secret::new("secret+/=密钥".into()).expect("non-whitespace secret is valid");
        assert_eq!(secret.as_ref(), "secret+/=密钥");
        assert_eq!(format!("{secret:?}"), "Secret(\"[redacted]\")");
        for input in ["", " secret", "secret\t", "secret\0", "secret\u{2003}"] {
            let error = Secret::new(input.into()).expect_err("invalid secret fails");
            assert_eq!(
                format!("{error:?}: {error}"),
                "SecretError: invalid PNVS access key secret"
            );
        }
    }

    #[test]
    fn pairs_validated_values_without_disclosing_them() {
        let access_key = AccessKey::new(
            Id::new("ExampleId123".into()).expect("id"),
            Secret::new("ExampleSecret".into()).expect("secret"),
        );
        assert_eq!(access_key.id().as_ref(), "ExampleId123");
        assert_eq!(access_key.secret().as_ref(), "ExampleSecret");
        assert_eq!(
            format!("{access_key:?}"),
            "AccessKey { id: Id(\"[redacted]\"), secret: Secret(\"[redacted]\") }"
        );
    }
}
