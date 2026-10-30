use std::{
    fmt::{self, Debug, Formatter},
    str::FromStr,
};

use bytes::BytesMut;
use derive_more::AsRef;
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};
use zeroize::Zeroizing;

pub use crate::identity::credential::ParseError;

/// The client-supplied secret that proves an original R1 request.
pub struct Secret(Zeroizing<[u8; 32]>);

impl Secret {
    /// Derive the original request proof.
    pub fn proof(&self) -> Proof {
        Proof(Sha256::digest(&self.0[..]).into())
    }
}

impl AsRef<[u8; 32]> for Secret {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
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

impl FromStr for Secret {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(crate::identity::credential::parse(input)?))
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let input = Zeroizing::<String>::deserialize(deserializer)?;
        input.parse().map_err(serde::de::Error::custom)
    }
}

/// The SHA-256 proof of the original client secret.
#[derive(AsRef)]
pub struct Proof([u8; 32]);

impl<'a> FromSql<'a> for Proof {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(<&[u8]>::from_sql(kind, raw)?.try_into()?))
    }
    fn accepts(kind: &Type) -> bool {
        <&[u8] as FromSql>::accepts(kind)
    }
}

impl ToSql for Proof {
    fn to_sql(
        &self,
        kind: &Type,
        output: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.0.as_slice().to_sql(kind, output)
    }
    fn accepts(kind: &Type) -> bool {
        <&[u8] as ToSql>::accepts(kind)
    }
    to_sql_checked!();
}

impl PartialEq for Proof {
    fn eq(&self, other: &Self) -> bool {
        bool::from(self.0.as_slice().ct_eq(other.0.as_slice()))
    }
}

impl Eq for Proof {}

impl Debug for Proof {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Proof").field(&"[redacted]").finish()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::identity::request::{ParseError, Secret};

    const ORIGINAL: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
    const BYTES: [u8; 32] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ];

    #[test]
    fn decodes_the_fixed_client_secret() {
        let secret = ORIGINAL.parse::<Secret>().expect("fixed secret is valid");
        assert_eq!(secret.as_ref(), &BYTES);
        assert_eq!(format!("{secret:?}"), "Secret(\"[redacted]\")");

        let url_safe = "__________________________________________8"
            .parse::<Secret>()
            .expect("URL-safe alphabet is valid");
        assert_eq!(url_safe.as_ref(), &[0xff; 32]);
    }

    #[test]
    fn deserializes_validated_secret_text_without_exposing_invalid_values() {
        for json in [
            r#""AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8""#,
            r#""\u0041AECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8""#,
        ] {
            let secret: Secret = serde_json::from_str(json).expect("canonical secret");
            assert_eq!(secret.as_ref(), &BYTES);
            let secret: Secret =
                serde_json::from_value(serde_json::from_str(json).expect("owned text"))
                    .expect("owned canonical secret");
            assert_eq!(secret.as_ref(), &BYTES);
        }
        for invalid in [
            "fictional-private-secret",
            "!AECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
        ] {
            let error = serde_json::from_value::<Secret>(serde_json::json!(invalid))
                .expect_err("invalid secret");
            assert!(!format!("{error:?}").contains(invalid));
            assert!(!error.to_string().contains(invalid));
        }
        for invalid in ["null", "123", "true", "[]", "{}"] {
            assert!(serde_json::from_str::<Secret>(invalid).is_err());
        }
    }

    #[test]
    fn derives_proof_from_decoded_bytes() {
        let secret = ORIGINAL.parse::<Secret>().expect("fixed secret is valid");
        let proof = secret.proof();
        let expected = [
            0x63, 0x0d, 0xcd, 0x29, 0x66, 0xc4, 0x33, 0x66, 0x91, 0x12, 0x54, 0x48, 0xbb, 0xb2,
            0x5b, 0x4f, 0xf4, 0x12, 0xa4, 0x9c, 0x73, 0x2d, 0xb2, 0xc8, 0xab, 0xc1, 0xb8, 0x58,
            0x1b, 0xd7, 0x10, 0xdd,
        ];

        assert_eq!(proof.as_ref(), &expected);
        assert_eq!(format!("{proof:?}"), "Proof(\"[redacted]\")");
    }

    #[test]
    fn proof_equality_matches_only_the_same_secret() {
        let original = ORIGINAL.parse::<Secret>().expect("fixed secret is valid");
        let same = ORIGINAL.parse::<Secret>().expect("fixed secret is valid");
        let other = "__________________________________________8"
            .parse::<Secret>()
            .expect("fixed other secret is valid");
        let proof = original.proof();

        assert_eq!(proof, same.proof());
        assert_ne!(proof, other.proof());
    }

    #[test]
    fn rejects_noncanonical_or_wrong_length_text() {
        for input in [
            "",
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh",
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
        ] {
            assert!(matches!(input.parse::<Secret>(), Err(ParseError::Length)));
        }

        for input in [
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh9",
            "!AECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
            "+AECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
        ] {
            assert!(matches!(
                input.parse::<Secret>(),
                Err(ParseError::Decode(_))
            ));
        }
    }
}
