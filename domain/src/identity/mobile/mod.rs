pub(crate) mod envelope;
pub mod hash;

use std::{
    fmt::{self, Debug, Formatter},
    str::FromStr,
};

use derive_more::AsRef;
use serde::{Deserialize, Deserializer};
use thiserror::Error;
use zeroize::Zeroizing;

/// A domestic mobile number accepted by the Identity entrypoints.
#[derive(Clone, AsRef)]
#[as_ref(str)]
pub struct Mobile(Zeroizing<String>);

impl Mobile {
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl Debug for Mobile {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Mobile")
            .field(&"[redacted]")
            .finish()
    }
}

impl FromStr for Mobile {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let bytes = input.as_bytes();

        if bytes.len() != 11 || !bytes.iter().all(u8::is_ascii_digit) || bytes[0] != b'1' {
            return Err(ParseError);
        }

        let accepted = matches!(
            (bytes[1], bytes[2]),
            (b'3' | b'8', _)
                | (b'4', b'0' | b'1' | b'4'..=b'9')
                | (b'5' | b'9', b'0'..=b'3' | b'5'..=b'9')
                | (b'6', b'2' | b'5'..=b'7')
                | (b'7', b'0'..=b'8')
        );

        if !accepted {
            return Err(ParseError);
        }

        Ok(Self(Zeroizing::new(input.to_owned())))
    }
}

impl<'de> Deserialize<'de> for Mobile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let input = Zeroizing::<String>::deserialize(deserializer)?;
        input.parse().map_err(serde::de::Error::custom)
    }
}

/// The supplied value is outside the accepted mobile format or prefix set.
#[derive(Debug, Error)]
#[error("invalid mobile")]
pub struct ParseError;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::identity::mobile::{Mobile, ParseError};

    #[test]
    fn deserializes_validated_mobile_text_without_exposing_invalid_values() {
        for json in [r#""13800000000""#, r#""\u00313800000000""#] {
            let mobile: Mobile = serde_json::from_str(json).expect("valid mobile");
            assert_eq!(mobile.as_ref(), "13800000000");
            let mobile: Mobile =
                serde_json::from_value(serde_json::from_str(json).expect("owned text"))
                    .expect("owned valid mobile");
            assert_eq!(mobile.as_ref(), "13800000000");
        }
        for invalid in ["+8613800000000", "13800000000 ", "１４８００００００００"] {
            let error = serde_json::from_value::<Mobile>(serde_json::json!(invalid))
                .expect_err("invalid mobile");
            assert!(!format!("{error:?}").contains(invalid));
            assert!(!error.to_string().contains(invalid));
        }
        for invalid in ["null", "123", "true", "[]", "{}"] {
            assert!(serde_json::from_str::<Mobile>(invalid).is_err());
        }
    }

    #[test]
    fn accepts_documented_prefix_boundaries() {
        for domestic in [
            "13000000000",
            "13900000000",
            "14000000000",
            "14100000000",
            "14400000000",
            "14900000000",
            "15000000000",
            "15300000000",
            "15500000000",
            "15900000000",
            "16200000000",
            "16500000000",
            "16700000000",
            "17000000000",
            "17800000000",
            "18000000000",
            "18900000000",
            "19000000000",
            "19300000000",
            "19500000000",
            "19900000000",
        ] {
            let parsed = domestic.parse::<Mobile>();
            assert!(
                matches!(&parsed, Ok(mobile) if mobile.as_ref() == domestic),
                "{parsed:?}"
            );
        }
    }

    #[test]
    fn rejects_country_prefixes_whitespace_and_separators() {
        for invalid in [
            "+8613800000000",
            "8613800000000",
            " 13800000000",
            "13800000000 ",
            "13800000000\n",
            "\t13800000000",
            "\u{2003}13800000000",
            "86 13800000000",
            "+86 13800000000",
            "138 0000 0000",
            "138-0000-0000",
            "008613800000000",
            "+8613800000000x",
        ] {
            assert!(matches!(invalid.parse::<Mobile>(), Err(ParseError)));
        }
    }

    #[test]
    fn rejects_unsupported_prefixes_and_non_ascii_digits() {
        for invalid in [
            "",
            " ",
            "23800000000",
            "1380000000a",
            "14200000000",
            "14300000000",
            "15400000000",
            "16100000000",
            "16400000000",
            "16800000000",
            "17900000000",
            "19400000000",
            "１２８００００００００",
            "1380000000",
            "138000000000",
        ] {
            assert!(matches!(invalid.parse::<Mobile>(), Err(ParseError)));
        }
    }

    #[test]
    fn debug_output_does_not_disclose_the_number() {
        let mobile = "13800000000".parse::<Mobile>();
        assert_eq!(format!("{mobile:?}"), "Ok(Mobile(\"[redacted]\"))");
    }
}
