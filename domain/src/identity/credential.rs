//! Shared byte representation; each owner fixes its credential type and envelope binding.

use std::{fmt, str::FromStr};

use base64::{DecodeSliceError, Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use platform::cryptography::envelope;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::identity::{keys, registration, session};

pub(crate) struct Secret(Zeroizing<[u8; 32]>);

/// Both request proofs and issued credentials use this canonical byte representation.
pub(crate) fn parse(input: &str) -> Result<Zeroizing<[u8; 32]>, ParseError> {
    if input.len() != 43 {
        return Err(ParseError::Length);
    }
    let mut bytes = Zeroizing::new([0; 32]);
    let decoded = URL_SAFE_NO_PAD.decode_slice(input, &mut bytes[..])?;
    if decoded != bytes.len() {
        return Err(ParseError::Length);
    }
    Ok(bytes)
}

impl FromStr for Secret {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(parse(input)?))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("secret must encode 32 bytes without padding")]
    Length,
    #[error("invalid secret encoding")]
    Decode(#[from] DecodeSliceError),
}

impl Secret {
    pub(crate) fn generate() -> Result<Self, getrandom::Error> {
        let mut value = Zeroizing::new([0; 32]);
        getrandom::fill(&mut value[..])?;
        Ok(Self(value))
    }
    pub(crate) fn text(&self) -> Zeroizing<String> {
        Zeroizing::new(URL_SAFE_NO_PAD.encode(&self.0[..]))
    }
    pub(crate) fn hash(&self) -> [u8; 32] {
        Sha256::digest(&self.0[..]).into()
    }
    pub(crate) fn matches(&self, hash: &[u8; 32]) -> bool {
        bool::from(self.hash().ct_eq(hash))
    }
    pub(crate) fn seal(
        &self,
        keys: &keys::credential::Keys,
        binding: Binding,
    ) -> Result<envelope::Envelope, Error> {
        struct Input<'a> {
            binding: Binding,
            secret: &'a Secret,
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
                &self.secret.0[..]
            }
        }
        Ok(keys.seal(&Input {
            binding,
            secret: self,
        })?)
    }
    pub(crate) fn open(
        keys: &keys::credential::Keys,
        envelope: &envelope::Envelope,
        binding: Binding,
    ) -> Result<Self, Error> {
        let bytes = keys.open(envelope, &binding)?;
        let mut value = Zeroizing::new([0; 32]);
        if bytes.len() != value.len() {
            return Err(Error::Length);
        }
        value.copy_from_slice(&bytes);
        Ok(Self(value))
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(..)")
    }
}

pub(crate) enum Binding {
    Handoff(session::Id),
    Registration(registration::Id),
}
impl envelope::Binding for Binding {
    fn domain(&self) -> &[u8] {
        b"nucleus/identity/envelope/v1"
    }
    fn identity(&self) -> &[u8] {
        match self {
            Self::Handoff(id) => id.as_bytes(),
            Self::Registration(id) => id.as_bytes(),
        }
    }
    fn field(&self) -> &[u8] {
        match self {
            Self::Handoff(_) => b"handoff.token",
            Self::Registration(_) => b"registration.token",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Key(#[from] keys::credential::Error),
    #[error("credential plaintext must contain exactly 32 bytes")]
    Length,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use platform::cryptography::envelope;
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use crate::{
        identity::{
            credential::{Binding, Secret},
            keys::credential::Keys,
        },
        keys::{Entry, Id, Material},
    };

    #[test]
    fn authenticates_independent_python_vectors_with_owner_and_identity_binding() {
        let id = Id::new(1).expect("key id");
        let keys = Keys::new(crate::keys::Derived::fixed(
            id,
            vec![Entry::new(
                id,
                Material(Zeroizing::new(std::array::from_fn(|index| {
                    u8::try_from(index).expect("byte")
                }))),
            )],
        ));
        let owner = Uuid::from_u128(3);
        for (handoff, encoded) in [
            (
                true,
                "010000000000000001000102030405060708090a0b64cd1c1bc9ece4ad714c73f195bb736400e3b483029ff269a235f6126ec13df8a95ac162c4447852956d79bb368d104c",
            ),
            (
                false,
                "010000000000000001000102030405060708090a0babf16f927efcfcabb51ff958dc52f4719520d4fb049c1e4217ee43531cf9c18c0ee013a383b4fec077277189485f63f5",
            ),
        ] {
            let binding = |id: Uuid| {
                if handoff {
                    Binding::Handoff(id.to_string().parse().expect("session identity"))
                } else {
                    Binding::Registration(id.to_string().parse().expect("registration identity"))
                }
            };
            let envelope =
                envelope::Envelope::new(hex::decode(encoded).expect("independent vector"))
                    .expect("supported envelope structure");
            let token =
                Secret::open(&keys, &envelope, binding(owner)).expect("authenticated vector");
            assert_eq!(
                &**token.text(),
                "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"
            );
            assert!(Secret::open(&keys, &envelope, binding(Uuid::from_u128(4))).is_err());
            let other = if handoff {
                Binding::Registration(owner.to_string().parse().expect("registration identity"))
            } else {
                Binding::Handoff(owner.to_string().parse().expect("session identity"))
            };
            assert!(Secret::open(&keys, &envelope, other).is_err());
        }
    }
}
