//! Identity key material invariants shared by the delivered purposes.

pub mod challenge;
pub mod database;
pub mod encryption;
pub mod input;
pub mod message;
pub mod metadata;
pub mod mobile;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
pub use platform::cryptography::key::Id;
use platform::cryptography::{self, Encoded, sha256};
use strum::{AsRefStr, EnumString};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio_postgres::types::{FromSql, Type};
use zeroize::Zeroizing;

pub struct Material(pub(crate) Zeroizing<[u8; 32]>);

impl Material {
    pub fn new(text: Zeroizing<String>) -> Result<Self, MaterialError> {
        if text.len() != 43 {
            return Err(MaterialError);
        }
        let mut material = Zeroizing::new([0; 32]);
        match URL_SAFE_NO_PAD.decode_slice(text.as_bytes(), &mut material[..]) {
            Ok(32) => Ok(Self(material)),
            _ => Err(MaterialError),
        }
    }
}

#[derive(Debug, Error)]
#[error("invalid key material")]
pub struct MaterialError;

pub struct Entry {
    pub(crate) id: Id,
    pub(crate) material: Material,
}

impl Entry {
    pub fn new(id: Id, material: Material) -> Self {
        Self { id, material }
    }
}

pub(crate) fn validate(active_id: Id, entries: &[Entry]) -> Result<(), Error> {
    if entries.len() > 256 {
        return Err(Error::Count);
    }
    for (index, entry) in entries.iter().enumerate() {
        for previous in &entries[..index] {
            if entry.id == previous.id {
                return Err(Error::DuplicateId(entry.id));
            }
            if bool::from(entry.material.0[..].ct_eq(&previous.material.0[..])) {
                return Err(Error::DuplicateMaterial {
                    first_id: previous.id,
                    second_id: entry.id,
                });
            }
        }
    }
    if !entries.iter().any(|entry| entry.id == active_id) {
        return Err(Error::Active(active_id));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, EnumString, AsRefStr)]
pub(crate) enum Purpose {
    #[strum(serialize = "challenge")]
    Challenge,
    #[strum(serialize = "mobile")]
    Mobile,
    #[strum(serialize = "encryption")]
    Encryption,
    #[strum(serialize = "message")]
    Message,
}

impl Purpose {
    fn bytes(&self) -> &[u8] {
        self.as_ref().as_bytes()
    }
}

impl<'a> FromSql<'a> for Purpose {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(<&str>::from_sql(kind, raw)?.parse()?)
    }

    fn accepts(kind: &Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

pub(crate) fn fingerprint(
    key_id: Id,
    purpose: Purpose,
    material: &[u8; 32],
) -> Result<sha256::Digest, cryptography::Error> {
    struct Input<'a> {
        key_id: [u8; 8],
        purpose: Purpose,
        material: &'a [u8; 32],
    }
    impl cryptography::Input<4> for Input<'_> {
        fn fields(&self) -> [&[u8]; 4] {
            [
                b"nucleus/identity/key/v1",
                &self.key_id,
                self.purpose.bytes(),
                self.material,
            ]
        }
    }
    Ok(sha256::Digest::new(&Encoded::new(&Input {
        key_id: key_id.as_ref().to_be_bytes(),
        purpose,
        material,
    })?))
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("key set exceeds 256 entries")]
    Count,
    #[error("duplicate key id {0:?}")]
    DuplicateId(Id),
    #[error("duplicate key material at {first_id:?} and {second_id:?}")]
    DuplicateMaterial { first_id: Id, second_id: Id },
    #[error("active key {0:?} is not declared")]
    Active(Id),
    #[error("cannot compute key fingerprint: {0}")]
    Crypto(#[from] cryptography::Error),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use tokio_postgres::types::{FromSql, Type};
    use zeroize::Zeroizing;

    use crate::identity::keys::{Entry, Error, Id, Material, Purpose, validate};

    #[test]
    fn constructs_only_canonical_material() {
        let material = Material::new(Zeroizing::new(
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8".into(),
        ))
        .expect("fixed bytes 0 through 31");
        assert_eq!(&material.0[..], &(0_u8..32).collect::<Vec<_>>());
        for text in [
            String::new(),
            "sensitive-material".into(),
            format!("{}=", "A".repeat(43)),
            "/".repeat(43),
            "+".repeat(43),
            format!("{}B", "A".repeat(42)),
            "A".repeat(42),
            "A".repeat(44),
        ] {
            let error = match Material::new(Zeroizing::new(text)) {
                Err(error) => error,
                Ok(_) => panic!("invalid material accepted"),
            };
            assert_eq!(format!("{error}"), "invalid key material");
        }
    }

    fn fixture(count: u16) -> Vec<Entry> {
        (0..count)
            .map(|id| {
                let mut material = [0; 32];
                material[..2].copy_from_slice(&id.to_be_bytes());
                Entry::new(
                    Id::new(i64::from(id)).expect("id"),
                    Material(Zeroizing::new(material)),
                )
            })
            .collect()
    }

    #[test]
    fn rejects_invalid_key_sets() {
        let id = Id::new(0).expect("id");
        let mut entries = fixture(2);
        entries[1].id = id;
        assert!(matches!(validate(id, &entries), Err(Error::DuplicateId(_))));
        let entries = vec![
            fixture(1).remove(0),
            Entry::new(Id::new(1).expect("id"), Material(Zeroizing::new([0; 32]))),
        ];
        assert!(matches!(
            validate(id, &entries),
            Err(Error::DuplicateMaterial { .. })
        ));
        assert!(matches!(validate(id, &[]), Err(Error::Active(_))));
        assert!(matches!(
            validate(Id::new(2).expect("id"), &fixture(1)),
            Err(Error::Active(_))
        ));
        assert!(validate(id, &fixture(256)).is_ok());
        assert!(matches!(validate(id, &fixture(257)), Err(Error::Count)));
        assert!(Id::new(-1).is_err());
        assert!(Id::new(i64::MAX).is_ok());
    }

    #[test]
    fn decodes_fixed_purpose_names() {
        for (purpose, expected) in [
            (Purpose::Challenge, "challenge"),
            (Purpose::Mobile, "mobile"),
            (Purpose::Encryption, "encryption"),
            (Purpose::Message, "message"),
        ] {
            assert_eq!(
                Purpose::from_sql(&Type::TEXT, expected.as_bytes()).expect("decode purpose"),
                purpose
            );
        }
    }

    #[test]
    fn rejects_unknown_purposes_invalid_text_and_wrong_sql_types() {
        let error = Purpose::from_sql(&Type::TEXT, b"fictional-sensitive-purpose")
            .expect_err("unknown purpose");
        assert!(!format!("{error:?}").contains("sensitive"));
        assert!(Purpose::from_sql(&Type::TEXT, &[0xff]).is_err());
        assert!(!<Purpose as FromSql>::accepts(&Type::INT4));
    }
}
