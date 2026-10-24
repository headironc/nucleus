pub mod client;
pub mod credentials;
pub mod proof;
pub mod response;
pub mod settings;
pub mod token;

use std::str::FromStr;

use bytes::BytesMut;
use derive_more::Display;
use strum::AsRefStr;
use thiserror::Error;
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};
use uuid::Uuid;

pub use crate::identity::challenge::token::Token;

/// The server-generated identity of a C1 human verification challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display)]
pub struct Id(Uuid);

impl<'a> FromSql<'a> for Id {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(Uuid::from_sql(kind, raw)?))
    }

    fn accepts(kind: &Type) -> bool {
        <Uuid as FromSql>::accepts(kind)
    }
}

impl ToSql for Id {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.0.to_sql(kind, out)
    }

    fn accepts(kind: &Type) -> bool {
        <Uuid as ToSql>::accepts(kind)
    }

    to_sql_checked!();
}

impl Id {
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }

    /// Create a new Challenge ID with UUIDv4.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl FromStr for Id {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(input)?))
    }
}

/// Why a Challenge ID cannot be reconstructed from text.
#[derive(Debug, Error)]
#[error("invalid UUID text")]
pub struct ParseError(#[from] uuid::Error);

/// A server-selected reason to issue a human verification challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, AsRefStr)]
pub enum Purpose {
    #[strum(serialize = "send_authentication_code")]
    Authentication,
    #[strum(serialize = "send_mobile_change_code")]
    MobileChange,
    #[strum(serialize = "send_mobile_binding_code")]
    MobileBinding,
}

impl<'a> FromSql<'a> for Purpose {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        match <&str>::from_sql(kind, raw)? {
            "authentication" => Ok(Self::Authentication),
            "mobile_change" => Ok(Self::MobileChange),
            "mobile_binding" => Ok(Self::MobileBinding),
            _ => Err(strum::ParseError::VariantNotFound.into()),
        }
    }

    fn accepts(kind: &Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

impl ToSql for Purpose {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        let purpose = match self {
            Self::Authentication => "authentication",
            Self::MobileChange => "mobile_change",
            Self::MobileBinding => "mobile_binding",
        };
        purpose.to_sql(kind, out)
    }

    fn accepts(kind: &Type) -> bool {
        <&str as ToSql>::accepts(kind)
    }

    to_sql_checked!();
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use bytes::BytesMut;
    use tokio_postgres::types::{FromSql, ToSql, Type};
    use uuid::{Uuid, Variant};

    use crate::identity::challenge::{Id, Purpose};

    #[test]
    fn generates_a_challenge_id() {
        let id = Id::new();
        let uuid = Uuid::parse_str(&id.to_string()).expect("generated ID is a UUID");

        assert_eq!(uuid.get_version_num(), 4);
        assert_eq!(uuid.get_variant(), Variant::RFC4122);
    }

    #[test]
    fn restores_uuid_values_and_normalizes_their_text() {
        for original in [
            "550e8400-e29b-41d4-a716-446655440000",
            "01890f6e-7b80-7000-8000-000000000000",
            "01890f6e-7b80-7000-7000-000000000000",
            "01890f6e-7b80-7000-c000-000000000000",
            "00000000-0000-0000-0000-000000000000",
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
        ] {
            let expected = original.parse::<Id>().expect("fixed UUID is valid");
            for text in [
                original.to_owned(),
                original.to_uppercase(),
                original.replace('-', ""),
                format!("{{{original}}}"),
                format!("urn:uuid:{original}"),
            ] {
                let parsed = text.parse::<Id>().expect("supported UUID text");
                assert_eq!(parsed, expected);
                assert_eq!(parsed.to_string(), original);
            }
        }
    }

    #[test]
    fn rejects_malformed_challenge_id_text() {
        for invalid in [
            "",
            "bad",
            "550e8400-e29b-41d4-a716-44665544000g",
            "550e8400-e29b-41d4-a716-44665544000",
            " 550e8400-e29b-41d4-a716-446655440000",
            "550e8400-e29b-41d4-a716-446655440000\n",
        ] {
            assert!(invalid.parse::<Id>().is_err());
        }
    }

    #[test]
    fn maps_server_purposes_to_fixed_turnstile_actions() {
        assert_eq!(Purpose::Authentication.as_ref(), "send_authentication_code");
        assert_eq!(Purpose::MobileChange.as_ref(), "send_mobile_change_code");
        assert_eq!(Purpose::MobileBinding.as_ref(), "send_mobile_binding_code");
    }

    #[test]
    fn encodes_and_restores_persisted_purpose_values() {
        for (purpose, stored) in [
            (Purpose::Authentication, "authentication"),
            (Purpose::MobileChange, "mobile_change"),
            (Purpose::MobileBinding, "mobile_binding"),
        ] {
            let mut output = BytesMut::new();
            purpose
                .to_sql_checked(&Type::TEXT, &mut output)
                .expect("encode persisted purpose");
            assert_eq!(&output[..], stored.as_bytes());
            assert_eq!(
                Purpose::from_sql(&Type::TEXT, stored.as_bytes())
                    .expect("decode persisted purpose"),
                purpose,
            );
            assert!(
                purpose
                    .to_sql_checked(&Type::BYTEA, &mut BytesMut::new())
                    .is_err()
            );
        }
    }

    #[test]
    fn rejects_invalid_persisted_purposes_without_exposing_input() {
        for text in [
            "",
            "fictional-sensitive-purpose",
            "Authentication",
            " authentication",
            "authentication ",
            Purpose::Authentication.as_ref(),
            Purpose::MobileChange.as_ref(),
            Purpose::MobileBinding.as_ref(),
        ] {
            let error = Purpose::from_sql(&Type::TEXT, text.as_bytes())
                .expect_err("only exact persisted purpose values are accepted");
            assert_eq!(format!("{error}"), "Matching variant not found");
            assert_eq!(format!("{error:?}"), "VariantNotFound");
        }
        assert!(Purpose::from_sql(&Type::TEXT, &[0xff]).is_err());
        assert!(Purpose::from_sql_nullable(&Type::TEXT, None).is_err());
        assert!(!<Purpose as FromSql>::accepts(&Type::INT4));
    }
}
