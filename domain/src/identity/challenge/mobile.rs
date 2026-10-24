//! Challenge-owned mobile ciphertext, bound to the original Challenge ID.

use std::fmt;

use bytes::BytesMut;
use derive_more::AsRef;
use platform::cryptography::{self, envelope};
use thiserror::Error;
use tokio_postgres::types::{FromSql, IsNull, Kind, ToSql, Type, to_sql_checked};

use crate::identity::{
    challenge,
    keys::{self, encryption},
    mobile::{self, hash::Hash},
};

#[derive(AsRef)]
#[as_ref([u8])]
pub struct Sealed(envelope::Envelope);

impl fmt::Debug for Sealed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Sealed(..)")
    }
}

impl<'a> FromSql<'a> for Sealed {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self::new(Vec::<u8>::from_sql(kind, raw)?)?)
    }

    fn accepts(kind: &Type) -> bool {
        Sealed::accepts_sql(kind)
    }
}

impl ToSql for Sealed {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().to_sql(kind, out)
    }

    fn accepts(kind: &Type) -> bool {
        Sealed::accepts_sql(kind)
    }

    to_sql_checked!();
}

impl Sealed {
    fn accepts_sql(kind: &Type) -> bool {
        match kind.kind() {
            Kind::Domain(r#type) => r#type == &Type::BYTEA,
            _ => kind == &Type::BYTEA,
        }
    }

    /// Restore the envelope layout; call `open` to authenticate it and validate its plaintext.
    pub fn new(bytes: Vec<u8>) -> Result<Self, envelope::ParseError> {
        Ok(Self(envelope::Envelope::new(bytes)?))
    }

    pub fn seal(
        keys: &encryption::Keys,
        challenge_id: challenge::Id,
        mobile: &mobile::Mobile,
    ) -> Result<Self, Error> {
        Ok(Self(keys.seal(&Input {
            binding: Binding(challenge_id),
            mobile,
        })?))
    }

    pub fn open(
        &self,
        keys: &encryption::Keys,
        challenge_id: challenge::Id,
    ) -> Result<mobile::Mobile, Error> {
        let plaintext = keys.open(&self.0, &Binding(challenge_id))?;
        Ok(std::str::from_utf8(&plaintext)?.parse()?)
    }

    pub fn key_id(&self) -> keys::Id {
        self.0.header().key_id()
    }
}

/// A Challenge's protected number; `open` authenticates its binding and mobile hash.
#[derive(Debug, FromSql, ToSql)]
#[postgres(name = "mobile")]
pub(crate) struct Mobile {
    value: Sealed,
    hash: Hash,
}

impl Mobile {
    pub(crate) fn seal(
        encryption: &encryption::Keys,
        mobile: &keys::mobile::Keys,
        challenge_id: challenge::Id,
        number: &mobile::Mobile,
    ) -> Result<Self, Error> {
        Ok(Self {
            value: Sealed::seal(encryption, challenge_id, number)?,
            hash: mobile.hash(number)?,
        })
    }

    pub(crate) fn open(
        &self,
        encryption: &encryption::Keys,
        mobile: &keys::mobile::Keys,
        challenge_id: challenge::Id,
    ) -> Result<mobile::Mobile, Error> {
        let number = self.value.open(encryption, challenge_id)?;
        if self.hash.as_ref() != mobile.hash(&number)?.as_ref() {
            return Err(Error::HashMismatch);
        }
        Ok(number)
    }

    pub(crate) fn matches(
        &self,
        mobile: &keys::mobile::Keys,
        number: &mobile::Mobile,
    ) -> Result<bool, cryptography::Error> {
        Ok(self.hash.as_ref() == mobile.hash(number)?.as_ref())
    }
}

struct Binding(challenge::Id);

struct Input<'a> {
    binding: Binding,
    mobile: &'a mobile::Mobile,
}

impl envelope::Input for Input<'_> {
    fn plaintext(&self) -> &[u8] {
        self.mobile.as_bytes()
    }
}

impl envelope::Binding for Input<'_> {
    fn domain(&self) -> &[u8] {
        self.binding.domain()
    }
    fn identity(&self) -> &[u8] {
        self.binding.identity()
    }
    fn field(&self) -> &[u8] {
        self.binding.field()
    }
}

impl envelope::Binding for Binding {
    fn domain(&self) -> &[u8] {
        b"nucleus/identity/envelope/v1"
    }
    fn identity(&self) -> &[u8] {
        self.0.0.as_bytes()
    }
    fn field(&self) -> &[u8] {
        b"challenge.mobile"
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("mobile hash computation failed")]
    Hash(#[from] cryptography::Error),
    #[error("stored mobile hash does not match the authenticated plaintext")]
    HashMismatch,
    #[error("{0}")]
    Encryption(#[from] encryption::Error),
    #[error("invalid UTF-8 in authenticated mobile plaintext")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("invalid authenticated mobile plaintext")]
    Mobile(#[from] mobile::ParseError),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use bytes::BytesMut;
    use platform::cryptography::envelope;
    use tokio_postgres::types::{FromSql, Kind, ToSql, Type};
    use zeroize::Zeroizing;

    use crate::identity::{
        challenge::{
            self,
            mobile::{Binding, Error, Sealed},
        },
        keys::{Entry, Id, Material, encryption},
    };

    fn entries() -> Vec<Entry> {
        vec![
            Entry::new(
                Id::new(0).expect("id"),
                Material(Zeroizing::new(std::array::from_fn(|i| {
                    u8::try_from(i).expect("32 bytes")
                }))),
            ),
            Entry::new(Id::new(1).expect("id"), Material(Zeroizing::new([42; 32]))),
        ]
    }

    fn challenge_id() -> challenge::Id {
        "01890f6e-7b80-7000-8000-000000000000"
            .parse()
            .expect("challenge id")
    }

    #[test]
    fn opens_independent_vector_with_fixed_challenge_binding() {
        // Independent Python AESGCMSIV vector also checks the Identity-owned AAD.
        let keys = encryption::Keys::new(Id::new(0).expect("id"), entries()).expect("keys");
        let bytes = hex::decode("010000000000000000000102030405060708090a0bb089c0edf060fbe8af85dc9060e507e1393773c7dabb6521ad283f").expect("fixture");
        let sealed = Sealed::new(bytes.clone()).expect("envelope");
        assert_eq!(
            sealed.open(&keys, challenge_id()).expect("open").as_ref(),
            "13800000000"
        );
        assert_eq!(sealed.as_ref(), bytes);
        assert!(matches!(
            sealed.open(&keys, challenge::Id::new()),
            Err(Error::Encryption(encryption::Error::Envelope(
                envelope::Error::Authentication(_)
            )))
        ));
    }

    #[test]
    fn accepts_bytea_domains_and_checks_envelope_structure() {
        let bytes = hex::decode("010000000000000000000102030405060708090a0bb089c0edf060fbe8af85dc9060e507e1393773c7dabb6521ad283f").expect("independent envelope fixture");
        let mut unsupported = bytes.clone();
        unsupported[0] = 2;
        for kind in [
            Type::BYTEA,
            Type::new(
                "sealed".into(),
                12345,
                Kind::Domain(Type::BYTEA),
                "identity".into(),
            ),
            Type::new(
                "sealed".into(),
                12346,
                Kind::Domain(Type::BYTEA),
                "other".into(),
            ),
            Type::new(
                "payload".into(),
                12347,
                Kind::Domain(Type::BYTEA),
                "other".into(),
            ),
        ] {
            assert!(<Sealed as FromSql>::accepts(&kind));
            let sealed = Sealed::from_sql(&kind, &bytes).expect("compatible byte representation");
            assert_eq!(sealed.as_ref(), bytes);
            let mut output = BytesMut::new();
            sealed
                .to_sql_checked(&kind, &mut output)
                .expect("compatible parameter");
            assert_eq!(&output[..], bytes);
            for invalid in [&[][..], &[1; 36], unsupported.as_slice()] {
                assert!(Sealed::from_sql(&kind, invalid).is_err());
            }
            assert!(Sealed::from_sql_nullable(&kind, None).is_err());
        }
        let sealed = Sealed::new(bytes).expect("valid envelope");
        for kind in [
            Type::UUID,
            Type::TEXT,
            Type::new(
                "sealed".into(),
                12348,
                Kind::Domain(Type::TEXT),
                "identity".into(),
            ),
        ] {
            assert!(!<Sealed as FromSql>::accepts(&kind));
            assert!(sealed.to_sql_checked(&kind, &mut BytesMut::new()).is_err());
        }
    }

    #[test]
    fn selects_original_key_without_falling_back_to_active() {
        let keys = encryption::Keys::new(Id::new(0).expect("id"), entries()).expect("keys");
        let mobile = "13800000000".parse().expect("mobile");
        let sealed = Sealed::seal(&keys, challenge_id(), &mobile).expect("seal");
        assert_eq!(*sealed.key_id().as_ref(), 0);
        let rotated = encryption::Keys::new(Id::new(1).expect("id"), entries()).expect("keys");
        assert_eq!(
            sealed
                .open(&rotated, challenge_id())
                .expect("retained")
                .as_ref(),
            mobile.as_ref()
        );
        assert_eq!(
            *Sealed::seal(&rotated, challenge_id(), &mobile)
                .expect("seal")
                .key_id()
                .as_ref(),
            1
        );
        let missing = encryption::Keys::new(Id::new(1).expect("id"), vec![entries().remove(1)])
            .expect("keys");
        assert!(matches!(
            sealed.open(&missing, challenge_id()),
            Err(Error::Encryption(encryption::Error::Missing(_)))
        ));
        let replaced = encryption::Keys::new(
            Id::new(0).expect("id"),
            vec![Entry::new(
                Id::new(0).expect("id"),
                Material(Zeroizing::new([42; 32])),
            )],
        )
        .expect("keys");
        assert!(matches!(
            sealed.open(&replaced, challenge_id()),
            Err(Error::Encryption(encryption::Error::Envelope(
                envelope::Error::Authentication(_)
            )))
        ));
    }

    #[test]
    fn rejects_authenticated_but_invalid_plaintext_without_disclosure() {
        struct Invalid<'a> {
            binding: Binding,
            plaintext: &'a [u8],
        }
        impl envelope::Binding for Invalid<'_> {
            fn domain(&self) -> &[u8] {
                self.binding.domain()
            }
            fn identity(&self) -> &[u8] {
                self.binding.identity()
            }
            fn field(&self) -> &[u8] {
                self.binding.field()
            }
        }
        impl envelope::Input for Invalid<'_> {
            fn plaintext(&self) -> &[u8] {
                self.plaintext
            }
        }
        let keys = encryption::Keys::new(Id::new(0).expect("id"), entries()).expect("keys");
        for plaintext in [
            b"+8613800000000".as_slice(),
            b"13800000000 ",
            b" 13800000000",
            b"14200000000",
            b"",
            &[255],
        ] {
            let sealed = Sealed(
                keys.seal(&Invalid {
                    binding: Binding(challenge_id()),
                    plaintext,
                })
                .expect("authenticated fixture"),
            );
            let error = sealed
                .open(&keys, challenge_id())
                .expect_err("invalid plaintext");
            if plaintext == [255] {
                assert!(matches!(error, Error::Utf8(_)));
            } else {
                assert!(matches!(error, Error::Mobile(_)));
            }
            assert!(!format!("{error:?} {error}").contains("13800000000"));
        }
    }
}
