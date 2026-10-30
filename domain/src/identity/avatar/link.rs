//! Canonical avatar identities, link settings and authenticated expiry.

use std::{str::FromStr, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use platform::cryptography::{self, Encoded};
use serde::Deserialize;
use serde_with::DeserializeFromStr;
use url::{Origin, Url};
use zeroize::Zeroizing;

use crate::identity::keys;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Digest([u8; 32]);

impl FromStr for Digest {
    type Err = ParseError;

    fn from_str(value: &str) -> Result<Self, ParseError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ParseError::Digest);
        }
        let mut bytes = [0; 32];
        hex::decode_to_slice(value, &mut bytes)?;
        Ok(Self(bytes))
    }
}

impl Digest {
    pub(crate) fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Canonical unsigned Unix seconds; authenticity and current time are checked later.
#[derive(DeserializeFromStr)]
pub struct Expiration(u64);

impl FromStr for Expiration {
    type Err = ParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let seconds: u64 = value.parse()?;
        if seconds.to_string() != value {
            return Err(ParseError::Canonical);
        }
        Ok(Self(seconds))
    }
}

/// An unverified avatar-link signature, distinct from a server-computed HMAC tag.
#[derive(Deserialize)]
#[serde(try_from = "Zeroizing<String>")]
pub struct Signature([u8; 32]);

impl TryFrom<Zeroizing<String>> for Signature {
    type Error = ParseError;

    fn try_from(value: Zeroizing<String>) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl FromStr for Signature {
    type Err = ParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 43 {
            return Err(ParseError::Signature);
        }
        let mut bytes = [0; 32];
        if URL_SAFE_NO_PAD.decode_slice(value, &mut bytes)? != bytes.len() {
            return Err(ParseError::Signature);
        }
        Ok(Self(bytes))
    }
}

pub struct Proof {
    digest: Digest,
    key_id: crate::keys::Id,
    expires_at: Expiration,
    signature: Signature,
}

impl Proof {
    pub fn new(
        digest: Digest,
        key_id: crate::keys::Id,
        expires_at: Expiration,
        signature: Signature,
    ) -> Self {
        Self {
            digest,
            key_id,
            expires_at,
            signature,
        }
    }

    pub(crate) fn verify(&self, keys: &keys::avatar::Keys) -> Result<(), Error> {
        let key = keys.find(self.key_id).ok_or(Error::Invalid)?;
        if !key.verify(
            &encoded(self.key_id, self.digest, self.expires_at.0)?,
            &self.signature.0,
        ) {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub(crate) fn digest(&self) -> Digest {
        self.digest
    }
    pub(crate) fn remaining(&self, now: DateTime<Utc>) -> Result<Duration, Error> {
        if u64::try_from(now.timestamp()).map_err(Error::Time)? >= self.expires_at.0 {
            return Err(Error::Invalid);
        }
        let expires = i64::try_from(self.expires_at.0).map_err(Error::Time)?;
        let expires = DateTime::from_timestamp(expires, 0).ok_or(Error::Deadline)?;
        (expires - now).to_std().map_err(Error::Remaining)
    }
}

pub struct Signed {
    href: String,
    expires_at: DateTime<Utc>,
}

impl Signed {
    pub fn href(&self) -> &str {
        &self.href
    }
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

#[derive(Clone)]
pub struct Link {
    origin: Origin,
    lifetime: Duration,
}

impl Link {
    pub fn new(origin: String, lifetime: Duration) -> Result<Self, SettingsError> {
        let (scheme, authority) = origin.split_once("://").ok_or(SettingsError::Origin)?;
        let authority = authority.strip_suffix('/').unwrap_or(authority);
        if !scheme.eq_ignore_ascii_case("https")
            || authority.is_empty()
            || authority.contains(['/', '@', '?', '#', '\\', '%'])
            || authority.ends_with(':')
            || origin
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(SettingsError::Origin);
        }
        let url = Url::parse(&origin)?;
        if url.scheme() != "https"
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(SettingsError::Origin);
        }
        if !(Duration::from_secs(60)..=Duration::from_secs(86400)).contains(&lifetime)
            || lifetime.subsec_nanos() != 0
        {
            return Err(SettingsError::Lifetime);
        }
        Ok(Self {
            origin: url.origin(),
            lifetime,
        })
    }

    pub fn default_lifetime() -> Duration {
        Duration::from_secs(86400)
    }

    pub(crate) fn sign(
        &self,
        keys: &keys::avatar::Keys,
        digest: Digest,
        now: DateTime<Utc>,
    ) -> Result<Signed, Error> {
        let expires = u64::try_from(now.timestamp())
            .map_err(Error::Time)?
            .checked_add(self.lifetime.as_secs())
            .ok_or(Error::Deadline)?;
        let expires_at = DateTime::from_timestamp(i64::try_from(expires).map_err(Error::Time)?, 0)
            .ok_or(Error::Deadline)?;
        let key = keys.active();
        let signature = key.sign(&encoded(key.id(), digest, expires)?);
        Ok(Signed {
            href: format!(
                "{}/api/v1/avatars/{}.webp?key_id={}&expires_at={expires}&signature={}",
                self.origin.ascii_serialization(),
                hex::encode(digest.0),
                key.id().as_ref(),
                URL_SAFE_NO_PAD.encode(signature.as_ref())
            ),
            expires_at,
        })
    }
}

fn encoded(
    key_id: crate::keys::Id,
    digest: Digest,
    expires_at: u64,
) -> Result<Encoded, cryptography::Error> {
    struct Input {
        key_id: [u8; 8],
        digest: Digest,
        expires_at: [u8; 8],
    }
    impl cryptography::Input<4> for Input {
        fn fields(&self) -> [&[u8]; 4] {
            [
                b"nucleus/avatar-link/v1",
                &self.key_id,
                &self.digest.0,
                &self.expires_at,
            ]
        }
    }
    Encoded::new(&Input {
        key_id: key_id.as_ref().to_be_bytes(),
        digest,
        expires_at: expires_at.to_be_bytes(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("avatar digest must be 64 lowercase hexadecimal characters")]
    Digest,
    #[error("avatar digest is malformed")]
    Hex(#[from] hex::FromHexError),
    #[error("avatar link number is malformed")]
    Number(#[from] std::num::ParseIntError),
    #[error("avatar link number is not canonical")]
    Canonical,
    #[error("avatar signature must encode exactly 32 bytes")]
    Signature,
    #[error("avatar signature encoding is invalid")]
    Decode(#[from] base64::DecodeSliceError),
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("avatar origin is not a URL")]
    Url(#[from] url::ParseError),
    #[error("avatar origin must be HTTPS without credentials, a path, query or fragment")]
    Origin,
    #[error("avatar link lifetime must contain 60 to 86400 whole seconds")]
    Lifetime,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("avatar link proof is invalid or expired")]
    Invalid,
    #[error("avatar link signature computation failed")]
    Crypto(#[from] cryptography::Error),
    #[error("avatar timestamp is outside the protocol range")]
    Time(#[source] std::num::TryFromIntError),
    #[error("avatar deadline is outside the timestamp range")]
    Deadline,
    #[error("avatar cache lifetime is negative or out of range")]
    Remaining(#[source] chrono::OutOfRangeError),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use chrono::{DateTime, TimeDelta};
    use zeroize::Zeroizing;

    use crate::{
        identity::{
            avatar::link::{Digest, Error, Expiration, Link, Proof, Signature, encoded},
            keys,
        },
        keys::{Entry, Id, Material},
    };

    fn entries() -> Vec<Entry> {
        vec![
            Entry::new(
                Id::new(7).expect("key id"),
                Material::new(Zeroizing::new(URL_SAFE_NO_PAD.encode(
                    std::array::from_fn::<_, 32, _>(|index| u8::try_from(index).expect("byte")),
                )))
                .expect("material"),
            ),
            Entry::new(
                Id::new(8).expect("next id"),
                Material::new(Zeroizing::new(URL_SAFE_NO_PAD.encode([42; 32]))).expect("material"),
            ),
        ]
    }

    #[test]
    fn independent_signature_bytes_retained_keys_and_exact_expiry() {
        // Independently derived with Python big-endian integer encoding, hashlib and hmac.
        let digest = Digest::new(std::array::from_fn(|index| {
            u8::try_from(index).expect("byte")
        }));
        let key_id = Id::new(7).expect("id");
        assert_eq!(
            hex::encode(
                encoded(key_id, digest, 1_700_000_000)
                    .expect("encoding")
                    .as_ref()
            ),
            "000000166e75636c6575732f6176617461722d6c696e6b2f763100000008000000000000000700000020000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f00000008000000006553f100"
        );
        let keys = keys::avatar::Keys::new(crate::keys::Derived::fixed(key_id, entries()));
        let signed = Link::new("https://example.com:443/".into(), Duration::from_secs(60))
            .expect("origin with port")
            .sign(
                &keys,
                digest,
                DateTime::from_timestamp(1_699_999_940, 0).expect("signing time"),
            )
            .expect("signed");
        assert_eq!(
            signed.href(),
            "https://example.com/api/v1/avatars/000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f.webp?key_id=7&expires_at=1700000000&signature=GESpzzRjRM44oFYcfOfkqjFP6-e5v0h7wsxWniSzUKw"
        );
        let proof = Proof::new(
            digest,
            "7".parse().expect("key id"),
            "1700000000".parse().expect("expiration"),
            "GESpzzRjRM44oFYcfOfkqjFP6-e5v0h7wsxWniSzUKw"
                .parse()
                .expect("signature"),
        );
        let before = signed.expires_at() - TimeDelta::nanoseconds(1);
        proof.verify(&keys).expect("original signature");
        assert_eq!(
            proof.remaining(before).expect("strictly before expiry"),
            Duration::from_nanos(1)
        );
        assert!(matches!(
            proof.remaining(signed.expires_at()),
            Err(Error::Invalid)
        ));
        let rotated = keys::avatar::Keys::new(crate::keys::Derived::fixed(
            Id::new(8).expect("id"),
            entries(),
        ));
        proof.verify(&rotated).expect("retained key verifies");
        let retired = keys::avatar::Keys::new(crate::keys::Derived::fixed(
            Id::new(8).expect("id"),
            vec![entries().remove(1)],
        ));
        assert!(matches!(proof.verify(&retired), Err(Error::Invalid)));
        for (digest, key, expires, signature) in [
            (
                Digest::new([0; 32]),
                "7",
                "1700000000",
                "GESpzzRjRM44oFYcfOfkqjFP6-e5v0h7wsxWniSzUKw",
            ),
            (
                digest,
                "8",
                "1700000000",
                "GESpzzRjRM44oFYcfOfkqjFP6-e5v0h7wsxWniSzUKw",
            ),
            (
                digest,
                "7",
                "1700000001",
                "GESpzzRjRM44oFYcfOfkqjFP6-e5v0h7wsxWniSzUKw",
            ),
            (
                digest,
                "7",
                "1700000000",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
        ] {
            assert!(matches!(
                Proof::new(
                    digest,
                    key.parse().expect("key id"),
                    expires.parse().expect("expiration"),
                    signature.parse().expect("signature"),
                )
                .verify(&keys),
                Err(Error::Invalid)
            ));
        }
    }

    #[test]
    fn rejects_repaired_origins_and_noncanonical_proof_fields() {
        for origin in [
            "https://example.com",
            "https://example.com/",
            "https://example.com:443",
            "https://example.com:8443/",
            "https://[::1]:443/",
        ] {
            Link::new(origin.into(), Duration::from_secs(60)).expect("HTTPS authority");
        }
        for origin in [
            "https://example.com/a/..",
            "https://example.com/%2e",
            "https://@example.com",
            "https://user@example.com",
            "https://example.com//",
            "https://example.com?",
            "https://example.com#",
            "https://example.com\\",
            "https://%65xample.com",
            "https://example.com:",
            " https://example.com",
            "https:/example.com",
            "http://example.com",
        ] {
            assert!(
                Link::new(origin.into(), Duration::from_secs(60)).is_err(),
                "{origin}"
            );
        }
        for digest in [
            "AA".repeat(32),
            "a".repeat(63),
            "a".repeat(65),
            "gg".repeat(32),
        ] {
            assert!(digest.parse::<Digest>().is_err());
        }
        for value in ["0", "1700000000", "18446744073709551615"] {
            let json = serde_json::to_string(value).expect("expiration text");
            let expires: Expiration = serde_json::from_str(&json).expect("canonical seconds");
            assert_eq!(expires.0.to_string(), value);
        }
        for value in [
            "",
            "-1",
            "+1",
            "01",
            "1.0",
            " 1",
            "1 ",
            "18446744073709551616",
        ] {
            assert!(value.parse::<Expiration>().is_err());
            let json = serde_json::to_string(value).expect("invalid expiration text");
            assert!(serde_json::from_str::<Expiration>(&json).is_err());
        }
        let signature: Signature =
            serde_json::from_str(r#""AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA""#)
                .expect("canonical signature");
        assert_eq!(signature.0, [0; 32]);
        for signature in [
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB",
            "",
            "sensitive-invalid-signature",
        ] {
            let error = signature
                .parse::<Signature>()
                .err()
                .expect("invalid signature");
            assert!(!format!("{error:?}").contains("sensitive-invalid-signature"));
            let json = serde_json::to_string(signature).expect("invalid signature text");
            for error in [
                serde_json::from_str::<Signature>(&json)
                    .err()
                    .expect("invalid borrowed signature"),
                serde_json::from_value::<Signature>(serde_json::json!(signature))
                    .err()
                    .expect("invalid owned signature"),
            ] {
                assert!(!format!("{error:?} {error}").contains("sensitive-invalid-signature"));
            }
        }
        for json in ["null", "123", "true", "[]", "{}"] {
            assert!(serde_json::from_str::<Expiration>(json).is_err());
            assert!(serde_json::from_str::<Signature>(json).is_err());
        }
    }
}
