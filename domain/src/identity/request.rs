use std::{
    fmt::{self, Debug, Formatter},
    str::FromStr,
};

use base64::{DecodeSliceError, Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use derive_more::AsRef;
use sha2::{Digest, Sha256};
use strum::{AsRefStr, EnumString};
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::Zeroizing;

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
        if input.len() != 43 {
            return Err(ParseError::Length);
        }

        let mut bytes = Zeroizing::new([0_u8; 32]);
        let decoded_len = URL_SAFE_NO_PAD.decode_slice(input, &mut bytes[..])?;
        if decoded_len != bytes.len() {
            return Err(ParseError::Length);
        }

        Ok(Self(bytes))
    }
}

/// The SHA-256 proof of the original client secret.
#[derive(AsRef)]
pub struct Proof([u8; 32]);

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

/// Why a client request secret cannot be reconstructed from text.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("request secret must encode 32 bytes without padding")]
    Length,
    #[error("invalid request secret encoding")]
    Decode(#[from] DecodeSliceError),
}

/// The client-declared session delivery for an original request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, AsRefStr)]
pub enum Transport {
    #[strum(serialize = "cookie")]
    Cookie,
    #[strum(serialize = "bearer")]
    Bearer,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::identity::request::{ParseError, Secret, Transport};

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

    #[test]
    fn accepts_only_exact_session_transport_values() {
        for (input, expected) in [("cookie", Transport::Cookie), ("bearer", Transport::Bearer)] {
            let transport = input
                .parse::<Transport>()
                .expect("fixed transport is valid");
            assert_eq!(transport, expected);
            assert_eq!(transport.as_ref(), input);
        }

        for invalid in [
            "",
            "Cookie",
            "BEARER",
            " cookie",
            "bearer ",
            "cookie,bearer",
        ] {
            assert!(matches!(
                invalid.parse::<Transport>(),
                Err(strum::ParseError::VariantNotFound)
            ));
        }
    }
}
