//! Versioned keys that seal permanent facts, and the shared seal encoding.

use std::collections::BTreeMap;

use platform::cryptography::{self, Encoded, hmac};
use thiserror::Error;

use crate::keys::{Derived, Id};

const DOMAIN: &[u8] = b"nucleus/seal/v1";

pub struct Keys {
    active: Key,
    retained: BTreeMap<Id, Key>,
}

struct Key {
    id: Id,
    material: hmac::Key,
}

impl Keys {
    pub(crate) fn new(derived: Derived) -> Self {
        let (active, retained) = derived.split(|id, material| Key {
            id,
            material: hmac::Key::new(material),
        });
        Self { active, retained }
    }

    /// History kinds use the same frozen prefix and field encoding as existing facts.
    pub(crate) fn sign(&self, kind: &str, version: i16, fields: &[&[u8]]) -> Result<Seal, Error> {
        Ok(Seal {
            tag: *self
                .active
                .material
                .sign(&encode_fields(kind, version, fields)?)
                .as_ref(),
            key_id: self.active.id,
            version,
        })
    }

    pub(crate) fn check(
        &self,
        kind: &str,
        version: i16,
        fields: &[&[u8]],
        seal: &Seal,
    ) -> Result<(), Error> {
        if seal.version != version {
            return Err(Error::Mismatch);
        }
        let key = if seal.key_id == self.active.id {
            &self.active
        } else {
            self.retained
                .get(&seal.key_id)
                .ok_or(Error::Missing(seal.key_id))?
        };
        if key
            .material
            .verify(&encode_fields(kind, version, fields)?, &seal.tag)
        {
            Ok(())
        } else {
            Err(Error::Mismatch)
        }
    }
}

pub(crate) fn encode_fields(
    kind: &str,
    version: i16,
    values: &[&[u8]],
) -> Result<Encoded, cryptography::Error> {
    let version = version.to_string();
    let mut fields: Vec<&[u8]> = Vec::with_capacity(values.len() + 3);
    fields.extend([DOMAIN, kind.as_bytes(), version.as_bytes()]);
    fields.extend_from_slice(values);
    Encoded::fields(&fields)
}

/// One fact's stored seal: the HMAC, the key that made it and the encoding version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seal {
    tag: [u8; 32],
    key_id: Id,
    version: i16,
}

impl Seal {
    /// Reads the stored seal columns; a malformed value is damage, never an absent seal.
    pub(crate) fn stored(tag: Vec<u8>, key_id: Id, version: i16) -> Result<Self, Error> {
        Ok(Self {
            tag: tag.try_into().map_err(|_| Error::Mismatch)?,
            key_id,
            version,
        })
    }

    pub(crate) fn tag(&self) -> &[u8] {
        &self.tag
    }

    pub(crate) fn key_id(&self) -> Id {
        self.key_id
    }

    pub(crate) fn version(&self) -> i16 {
        self.version
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("missing fact seal key {0:?}")]
    Missing(Id),
    #[error("fact seal does not authenticate its fact")]
    Mismatch,
    #[error("cannot encode sealed fact: {0}")]
    Encoding(#[from] cryptography::Error),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use platform::cryptography::sha256;
    use zeroize::Zeroizing;

    use crate::keys::{
        Derived, Entry, Id, Material,
        seal::{Error, Keys, Seal, encode_fields},
    };

    fn keys(active: i64, entries: &[(i64, u8)]) -> Keys {
        Keys::new(Derived::fixed(
            Id::new(active).expect("active"),
            entries
                .iter()
                .map(|(id, byte)| {
                    Entry::new(
                        Id::new(*id).expect("id"),
                        Material(Zeroizing::new(std::array::from_fn(|index| {
                            u8::try_from(index).expect("byte") ^ byte
                        }))),
                    )
                })
                .collect(),
        ))
    }

    #[test]
    fn seals_with_the_frozen_prefix_and_verifies_with_the_recorded_key() {
        // A fixed seal-use key of bytes 0..31; root derivation has its own vectors.
        let original = keys(0, &[(0, 0)]);
        let fields: &[&[u8]] = &[b"a", b""];
        let encoded = encode_fields("test.fixture", 1, fields).expect("encoding");
        // Python struct.pack('>I'), hashlib and hmac over the seal domain, type, version and fields.
        assert_eq!(encoded.as_ref().len(), 49);
        assert_eq!(
            hex::encode(sha256::Digest::new(&encoded).as_ref()),
            "62042130a3c3771d283fe8ad4c19d70d4a15e53b9708f5fad48ee0f7fa20d59f"
        );
        let seal = original.sign("test.fixture", 1, fields).expect("seal");
        assert_eq!(
            hex::encode(seal.tag()),
            "a0fbccebfbc267f09139e99d2c1f7fba8bada3cc2f0207083500636ad9cf6a01"
        );
        assert_eq!(
            (seal.key_id(), seal.version()),
            (Id::new(0).expect("id"), 1)
        );
        original
            .check("test.fixture", 1, fields, &seal)
            .expect("own seal");
        let rotated = keys(1, &[(0, 0), (1, 99)]);
        rotated
            .check("test.fixture", 1, fields, &seal)
            .expect("retained key verifies");
        assert!(matches!(
            rotated.check("test.fixture", 1, &[b"", b"a"], &seal),
            Err(Error::Mismatch)
        ));
        let other = Seal::stored(seal.tag().to_vec(), seal.key_id(), 2).expect("stored");
        assert!(matches!(
            rotated.check("test.fixture", 1, fields, &other),
            Err(Error::Mismatch)
        ));
        let absent = keys(1, &[(1, 99)]);
        assert!(matches!(
            absent.check("test.fixture", 1, fields, &seal),
            Err(Error::Missing(_))
        ));
        assert!(Seal::stored(vec![0; 31], seal.key_id(), 1).is_err());
    }
}
