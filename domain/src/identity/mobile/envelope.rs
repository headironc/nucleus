//! A mobile ciphertext and digest authenticated together under their record's binding.

use platform::cryptography::envelope;
use tokio_postgres::types::{FromSql, ToSql};

use crate::{
    database::sealed::Sealed,
    identity::{
        account, challenge, keys,
        mobile::{Mobile, hash::Hash},
        registration,
    },
};

#[derive(Debug, FromSql, ToSql)]
#[postgres(name = "mobile")]
pub(crate) struct Envelope {
    value: Sealed,
    hash: Hash,
}

pub(crate) enum Binding {
    Challenge(challenge::Id),
    Account(account::Id),
    Registration(registration::Id),
    Attempt(registration::attempt::Id),
}

impl envelope::Binding for Binding {
    fn domain(&self) -> &[u8] {
        b"nucleus/identity/envelope/v1"
    }
    fn identity(&self) -> &[u8] {
        match self {
            Self::Challenge(id) => id.as_bytes(),
            Self::Account(id) => id.as_bytes(),
            Self::Registration(id) => id.as_bytes(),
            Self::Attempt(id) => id.as_bytes(),
        }
    }
    fn field(&self) -> &[u8] {
        match self {
            Self::Challenge(_) => b"challenge.mobile",
            Self::Account(_) => b"account.mobile",
            Self::Registration(_) => b"registration.mobile",
            Self::Attempt(_) => b"attempt.mobile",
        }
    }
}

impl Envelope {
    pub(in crate::identity) fn ciphertext(&self) -> &envelope::Envelope {
        &self.value.0
    }

    pub(crate) fn hash(&self) -> &Hash {
        &self.hash
    }

    pub(crate) fn seal(
        encryption: &keys::encryption::Keys,
        lookup: &keys::mobile::Keys,
        binding: Binding,
        mobile: &Mobile,
    ) -> Result<Self, Error> {
        struct Input<'a> {
            binding: Binding,
            mobile: &'a Mobile,
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
        impl envelope::Input for Input<'_> {
            fn plaintext(&self) -> &[u8] {
                self.mobile.as_bytes()
            }
        }
        let value = encryption.seal(&Input { binding, mobile })?;
        Ok(Self {
            value: Sealed(value),
            hash: lookup.hash(mobile)?,
        })
    }

    pub(crate) fn open(
        &self,
        encryption: &keys::encryption::Keys,
        lookup: &keys::mobile::Keys,
        binding: Binding,
    ) -> Result<Mobile, Error> {
        let plaintext = encryption.open(&self.value.0, &binding)?;
        let mobile: Mobile = std::str::from_utf8(&plaintext)?.parse()?;
        if self.hash.as_ref() != lookup.hash(&mobile)?.as_ref() {
            return Err(Error::HashMismatch);
        }
        Ok(mobile)
    }

    pub(crate) fn matches(
        &self,
        keys: &keys::mobile::Keys,
        mobile: &Mobile,
    ) -> Result<bool, platform::cryptography::Error> {
        Ok(self.hash.as_ref() == keys.hash(mobile)?.as_ref())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Encryption(#[from] keys::encryption::Error),
    #[error("mobile plaintext is not UTF-8")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("mobile plaintext is invalid")]
    Mobile(#[from] crate::identity::mobile::ParseError),
    #[error("mobile lookup computation failed")]
    Hash(#[from] platform::cryptography::Error),
    #[error("mobile lookup does not match authenticated plaintext")]
    HashMismatch,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use platform::cryptography::envelope;
    use tokio_postgres::types::{FromSql, Type};
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use crate::{
        database::sealed::Sealed,
        identity::{
            challenge, keys,
            mobile::envelope::{Binding, Envelope, Error},
        },
        keys::{Entry, Id, Material},
    };

    fn entries() -> Vec<Entry> {
        vec![
            Entry::new(
                Id::new(0).expect("id"),
                Material(Zeroizing::new(std::array::from_fn(|index| {
                    u8::try_from(index).expect("32 bytes")
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
    fn authenticates_challenge_vector_and_its_mobile_digest() {
        let keys = keys::encryption::Keys::new(crate::keys::Derived::fixed(
            Id::new(0).expect("id"),
            entries(),
        ));
        let hashes = keys::mobile::Keys::new(crate::keys::Derived::fixed(
            Id::new(0).expect("id"),
            entries(),
        ));
        let mobile = "13800000000".parse().expect("mobile");
        // Independent Python AESGCMSIV vector checks the original Challenge AAD.
        let bytes = hex::decode("010000000000000000000102030405060708090a0bb089c0edf060fbe8af85dc9060e507e1393773c7dabb6521ad283f").expect("vector");
        let mut value = Envelope {
            value: Sealed(envelope::Envelope::new(bytes.clone()).expect("envelope")),
            hash: hashes.hash(&mobile).expect("hash"),
        };
        assert_eq!(
            value
                .open(&keys, &hashes, Binding::Challenge(challenge_id()))
                .expect("open")
                .as_ref(),
            "13800000000"
        );
        assert_eq!(value.value.0.as_ref(), bytes);
        assert!(matches!(
            value.open(&keys, &hashes, Binding::Challenge(challenge::Id::new())),
            Err(Error::Encryption(keys::encryption::Error::Envelope(
                envelope::Error::Authentication(_)
            )))
        ));
        value.hash = hashes
            .hash(&"13900000000".parse().expect("other mobile"))
            .expect("hash");
        assert!(matches!(
            value.open(&keys, &hashes, Binding::Challenge(challenge_id())),
            Err(Error::HashMismatch)
        ));
    }

    #[test]
    fn selects_original_key_without_falling_back_to_active() {
        let keys = keys::encryption::Keys::new(crate::keys::Derived::fixed(
            Id::new(0).expect("id"),
            entries(),
        ));
        let hashes = keys::mobile::Keys::new(crate::keys::Derived::fixed(
            Id::new(0).expect("id"),
            entries(),
        ));
        let mobile = "13800000000".parse().expect("mobile");
        let value = Envelope::seal(&keys, &hashes, Binding::Challenge(challenge_id()), &mobile)
            .expect("seal");
        assert_eq!(*value.value.0.header().key_id().as_ref(), 0);
        let rotated = keys::encryption::Keys::new(crate::keys::Derived::fixed(
            Id::new(1).expect("id"),
            entries(),
        ));
        assert_eq!(
            value
                .open(&rotated, &hashes, Binding::Challenge(challenge_id()))
                .expect("retained")
                .as_ref(),
            mobile.as_ref()
        );
        let current = Envelope::seal(
            &rotated,
            &hashes,
            Binding::Challenge(challenge_id()),
            &mobile,
        )
        .expect("seal");
        assert_eq!(*current.value.0.header().key_id().as_ref(), 1);
        let missing = keys::encryption::Keys::new(crate::keys::Derived::fixed(
            Id::new(1).expect("id"),
            vec![entries().remove(1)],
        ));
        assert!(matches!(
            value.open(&missing, &hashes, Binding::Challenge(challenge_id())),
            Err(Error::Encryption(keys::encryption::Error::Missing(_)))
        ));
        let replaced = keys::encryption::Keys::new(crate::keys::Derived::fixed(
            Id::new(0).expect("id"),
            vec![Entry::new(
                Id::new(0).expect("id"),
                Material(Zeroizing::new([42; 32])),
            )],
        ));
        assert!(matches!(
            value.open(&replaced, &hashes, Binding::Challenge(challenge_id())),
            Err(Error::Encryption(keys::encryption::Error::Envelope(
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
        let keys = keys::encryption::Keys::new(crate::keys::Derived::fixed(
            Id::new(0).expect("id"),
            entries(),
        ));
        let hashes = keys::mobile::Keys::new(crate::keys::Derived::fixed(
            Id::new(0).expect("id"),
            entries(),
        ));
        for plaintext in [
            b"+8613800000000".as_slice(),
            b"13800000000 ",
            b" 13800000000",
            b"14200000000",
            b"",
            &[255],
        ] {
            let value = Envelope {
                value: Sealed(
                    keys.seal(&Invalid {
                        binding: Binding::Challenge(challenge_id()),
                        plaintext,
                    })
                    .expect("authenticated fixture"),
                ),
                hash: hashes
                    .hash(&"13800000000".parse().expect("mobile"))
                    .expect("hash"),
            };
            let error = value
                .open(&keys, &hashes, Binding::Challenge(challenge_id()))
                .expect_err("invalid plaintext");
            if plaintext == [255] {
                assert!(matches!(error, Error::Utf8(_)));
            } else {
                assert!(matches!(error, Error::Mobile(_)));
            }
            assert!(!format!("{error:?} {error}").contains("13800000000"));
        }
    }

    #[test]
    fn opens_independent_attempt_vector_only_for_the_original_attempt() {
        let keys = keys::encryption::Keys::new(crate::keys::Derived::fixed(
            Id::new(1).expect("id"),
            vec![Entry::new(
                Id::new(1).expect("id"),
                entries().remove(0).material,
            )],
        ));
        let hashes = keys::mobile::Keys::new(crate::keys::Derived::fixed(
            Id::new(0).expect("id"),
            entries(),
        ));
        let uuid =
            Uuid::parse_str("00000000-0000-4000-8000-000000000004").expect("fixture identity");
        let id = crate::identity::registration::attempt::Id::from_sql(&Type::UUID, uuid.as_bytes())
            .expect("attempt identity");
        let value = Envelope {
            value: Sealed(envelope::Envelope::new(hex::decode("010000000000000001000102030405060708090a0bfd4204044e6a758ffb5d6d1620a693d1b3dd13880d595e32d6b4cf").expect("vector")).expect("envelope")),
            hash: hashes.hash(&"13800138000".parse().expect("mobile")).expect("hash"),
        };
        assert_eq!(
            value
                .open(&keys, &hashes, Binding::Attempt(id))
                .expect("original attempt")
                .as_ref(),
            "13800138000"
        );
        assert!(
            value
                .open(
                    &keys,
                    &hashes,
                    Binding::Account(
                        "00000000-0000-4000-8000-000000000004"
                            .parse()
                            .expect("account")
                    )
                )
                .is_err()
        );
    }

    #[test]
    fn opens_independent_vectors_only_under_the_original_record_binding() {
        let id = Id::new(1).expect("key id");
        let entry = || {
            vec![Entry::new(
                id,
                Material(Zeroizing::new(std::array::from_fn(|index| {
                    u8::try_from(index).expect("byte")
                }))),
            )]
        };
        let encryption = keys::encryption::Keys::new(crate::keys::Derived::fixed(id, entry()));
        let lookup = keys::mobile::Keys::new(crate::keys::Derived::fixed(id, entry()));
        let owner = Uuid::from_u128(3);
        let mobile = "13800138000".parse().expect("mobile");
        for (account, encoded) in [
            (
                true,
                "010000000000000001000102030405060708090a0b57012e36ff0efbe314be02ccbcb8fc9dd3ef25246edf32ed2d9c1f",
            ),
            (
                false,
                "010000000000000001000102030405060708090a0bab6185160a3fb7617d51d55b915de79c92eac4857cb4fda0721c98",
            ),
        ] {
            let binding = |id: Uuid| {
                if account {
                    Binding::Account(id.to_string().parse().expect("account identity"))
                } else {
                    Binding::Registration(id.to_string().parse().expect("registration identity"))
                }
            };
            let protected = Envelope {
                value: Sealed(
                    envelope::Envelope::new(hex::decode(encoded).expect("vector")).expect("layout"),
                ),
                hash: lookup.hash(&mobile).expect("hash"),
            };
            assert_eq!(
                protected
                    .open(&encryption, &lookup, binding(owner))
                    .expect("authenticated vector")
                    .as_bytes(),
                mobile.as_bytes()
            );
            assert!(
                protected
                    .open(&encryption, &lookup, binding(Uuid::from_u128(4)))
                    .is_err()
            );
            let other = if account {
                Binding::Registration(owner.to_string().parse().expect("registration identity"))
            } else {
                Binding::Account(owner.to_string().parse().expect("account identity"))
            };
            assert!(protected.open(&encryption, &lookup, other).is_err());
        }
    }
}
