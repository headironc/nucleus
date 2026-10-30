pub mod client;
pub mod credentials;
pub mod limits;
pub mod proof;
pub mod request;
pub mod response;
pub mod settings;
pub(crate) mod worker;

use std::{
    fmt::{self, Debug, Formatter},
    str::FromStr,
};

use chrono::{DateTime, Utc};
use derive_more::{AsRef, Display};
use serde::{Deserialize, Deserializer, Serialize};
use serde_with::DeserializeFromStr;
use thiserror::Error;
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::Zeroizing;

pub use crate::identity::message::limits::Limits;

#[derive(Clone, Copy)]
pub(crate) enum Conclusion {
    Sent,
    Failed,
    Unknown,
}

impl Conclusion {
    pub(crate) fn at(self, finished_at: DateTime<Utc>) -> Status {
        match self {
            Self::Sent => Status::Sent { finished_at },
            Self::Failed => Status::Failed { finished_at },
            Self::Unknown => Status::Unknown { finished_at },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum Status {
    Sending,
    Sent { finished_at: DateTime<Utc> },
    Failed { finished_at: DateTime<Utc> },
    Unknown { finished_at: DateTime<Utc> },
}

impl Status {
    pub(crate) fn finished_at(self) -> Option<DateTime<Utc>> {
        match self {
            Self::Sending => None,
            Self::Sent { finished_at }
            | Self::Failed { finished_at }
            | Self::Unknown { finished_at } => Some(finished_at),
        }
    }
}

/// The server-generated identity of a C1 message and its paired R1 check.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Display,
    Serialize,
    DeserializeFromStr,
    ToSchema,
    FromSql,
    ToSql,
)]
#[serde(transparent)]
#[schema(as = message::Id)]
#[postgres(transparent)]
pub struct Id(Uuid);

impl Id {
    /// Create a new Message ID with UUIDv4.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl FromStr for Id {
    type Err = ParseIdError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(input)?))
    }
}

/// Why a Message ID cannot be reconstructed from text.
#[derive(Debug, Error)]
#[error("invalid UUID text")]
pub struct ParseIdError(#[from] uuid::Error);

/// The six-digit code shared by authentication and mobile-change messages.
#[derive(AsRef)]
#[as_ref(str)]
pub struct Code(Zeroizing<String>);

impl Code {
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Generate a code from the operating system's random source.
    pub fn generate() -> Result<Self, getrandom::Error> {
        Self::generate_with(getrandom::fill)
    }

    fn generate_with(
        mut fill: impl FnMut(&mut [u8]) -> Result<(), getrandom::Error>,
    ) -> Result<Self, getrandom::Error> {
        loop {
            let mut bytes = Zeroizing::new([0_u8; 4]);
            fill(&mut bytes[..])?;
            let sample = u32::from_le_bytes(*bytes);
            // Accept a whole number of million-value ranges to avoid modulo bias.
            if sample < 4_294_000_000 {
                return Ok(Self(Zeroizing::new(format!("{:06}", sample % 1_000_000))));
            }
        }
    }
}

impl FromStr for Code {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.len() != 6 || !input.bytes().all(|digit| digit.is_ascii_digit()) {
            return Err(ParseError);
        }

        Ok(Self(Zeroizing::new(input.to_owned())))
    }
}

impl Debug for Code {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Code").field(&"[redacted]").finish()
    }
}

impl<'de> Deserialize<'de> for Code {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let input = Zeroizing::<String>::deserialize(deserializer)?;
        input.parse().map_err(serde::de::Error::custom)
    }
}

/// The supplied message code is not six ASCII digits.
#[derive(Debug, Error)]
#[error("invalid message code")]
pub struct ParseError;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use uuid::{Uuid, Variant};

    use crate::identity::message::{Code, Id, ParseError};

    #[test]
    fn generates_a_message_id() {
        let id = Id::new();
        let text = id.to_string();
        let uuid = Uuid::parse_str(&text).expect("generated ID is a UUID");

        assert_eq!(uuid.get_version_num(), 4);
        assert_eq!(uuid.get_variant(), Variant::RFC4122);
        assert_eq!(uuid.hyphenated().to_string(), text);
    }

    #[test]
    fn parses_and_deserializes_uuid_values_and_normalizes_their_text() {
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
    fn rejects_non_string_or_malformed_message_ids() {
        for invalid in ["null", "123", "true", "[]", "{}", r#""bad""#] {
            assert!(serde_json::from_str::<Id>(invalid).is_err());
        }
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
    fn accepts_exactly_six_ascii_digits_and_keeps_leading_zeroes() {
        for text in ["000000", "001234", "999999"] {
            let code = text.parse::<Code>().expect("six ASCII digits are valid");
            assert_eq!(code.as_ref(), text);
        }

        for invalid in [
            "",
            "12345",
            "1234567",
            "12345678",
            "123456789",
            " 123456",
            "123456\n",
            "123 56",
            "123-56",
            "１２３４５６",
            "１２",
            "123٤5",
        ] {
            assert!(matches!(invalid.parse::<Code>(), Err(ParseError)));
        }
    }

    #[test]
    fn deserializes_validated_code_text_without_exposing_invalid_values() {
        for json in [r#""001234""#, r#""\u003001234""#] {
            let code: Code = serde_json::from_str(json).expect("valid code");
            assert_eq!(code.as_ref(), "001234");
            let code: Code =
                serde_json::from_value(serde_json::from_str(json).expect("owned text"))
                    .expect("owned valid code");
            assert_eq!(code.as_ref(), "001234");
        }
        for invalid in ["1234567", " 123456", "１２３４５６"] {
            let error = serde_json::from_value::<Code>(serde_json::json!(invalid))
                .expect_err("invalid code");
            assert!(!format!("{error:?}").contains(invalid));
            assert!(!error.to_string().contains(invalid));
        }
        for invalid in ["null", "123", "true", "[]", "{}"] {
            assert!(serde_json::from_str::<Code>(invalid).is_err());
        }
    }

    #[test]
    fn rejection_sampling_skips_out_of_range_values_and_preserves_leading_zeroes() {
        let samples = [4_294_000_000_u32, u32::MAX, 0];
        let mut calls = 0;
        let code = Code::generate_with(|bytes| {
            bytes.copy_from_slice(&samples[calls].to_le_bytes());
            calls += 1;
            Ok(())
        })
        .expect("fixed third sample is accepted");

        assert_eq!(calls, 3);
        assert_eq!(code.as_ref(), "000000");

        for (sample, expected) in [(4_293_999_999_u32, "999999"), (1_000_001_u32, "000001")] {
            let code = Code::generate_with(|bytes| {
                bytes.copy_from_slice(&sample.to_le_bytes());
                Ok(())
            })
            .expect("fixed sample is accepted");
            assert_eq!(code.as_ref(), expected);
        }
    }

    #[test]
    fn random_source_failure_produces_no_code() {
        let result = Code::generate_with(|bytes| {
            bytes.copy_from_slice(&123456_u32.to_le_bytes());
            Err(getrandom::Error::UNSUPPORTED)
        });
        assert!(matches!(result, Err(getrandom::Error::UNSUPPORTED)));
    }

    #[test]
    fn debug_output_does_not_disclose_the_code() {
        let code = "001234".parse::<Code>();
        assert_eq!(format!("{code:?}"), "Ok(Code(\"[redacted]\"))");
    }
}
