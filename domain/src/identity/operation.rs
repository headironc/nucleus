use std::str::FromStr;

use bytes::BytesMut;
use derive_more::Display;
use serde::Serialize;
use serde_with::DeserializeFromStr;
use thiserror::Error;
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};
use utoipa::ToSchema;
use uuid::Uuid;

/// The client-supplied identifier of an original Identity operation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Display, Serialize, DeserializeFromStr, ToSchema,
)]
#[serde(transparent)]
#[schema(as = operation::Id)]
pub struct Id(Uuid);

impl Id {
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

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

impl FromStr for Id {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(input)?))
    }
}

/// Why an operation identifier cannot be constructed.
#[derive(Debug, Error)]
#[error("invalid UUID text")]
pub struct ParseError(#[from] uuid::Error);

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::error::Error;

    use crate::identity::operation::Id;

    #[test]
    fn rejects_non_string_or_malformed_input() {
        for invalid in ["null", "123", "true", "[]", "{}", r#""not-a-uuid""#] {
            assert!(serde_json::from_str::<Id>(invalid).is_err());
        }
    }

    #[test]
    fn parses_and_deserializes_uuid_values_without_format_version_or_variant_restrictions() {
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
                assert_eq!(serde_json::to_value(parsed).expect("UUID output"), original);
                let json = serde_json::to_string(&text).expect("UUID text serializes");
                assert_eq!(
                    serde_json::from_str::<Id>(&json).expect("UUID string"),
                    expected
                );
            }
        }
    }

    #[test]
    fn rejects_malformed_uuid_text_and_preserves_the_parser_cause() {
        for invalid in [
            "not-a-uuid",
            "550e8400-e29b-41d4-a716-44665544000g",
            "550e8400-e29b-41d4-a716-44665544000",
            " 01890f6e-7b80-7000-8000-000000000000",
            "01890f6e-7b80-7000-8000-000000000000\n",
            "",
        ] {
            let error = invalid.parse::<Id>().expect_err("malformed UUID");
            assert_eq!(error.to_string(), "invalid UUID text");
            assert!(
                error
                    .source()
                    .expect("parser cause retained")
                    .is::<uuid::Error>()
            );
        }
    }
}
