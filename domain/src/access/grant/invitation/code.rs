//! The original invitation code, its lookup digest, and its sealed storage binding.

use std::fmt;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use platform::cryptography::{self, Encoded, envelope};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

use crate::{
    access::grant::{
        self, Error,
        invitation::{
            Id,
            keys::{self, Keys},
        },
    },
    database::sealed::Sealed,
};

/// Thirty-two CSPRNG bytes, disclosed only as unpadded Base64URL text.
pub struct Code(Zeroizing<[u8; 32]>);

impl fmt::Debug for Code {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Code(..)")
    }
}

impl Serialize for Code {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text())
    }
}

impl<'de> Deserialize<'de> for Code {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = Zeroizing::<String>::deserialize(deserializer)?;
        let mut bytes = Zeroizing::new([0; 32]);
        if text.len() != 43
            || URL_SAFE_NO_PAD
                .decode_slice(text.as_bytes(), &mut bytes[..])
                .ok()
                != Some(32)
        {
            return Err(serde::de::Error::custom("invalid invitation code"));
        }
        Ok(Self(bytes))
    }
}

impl Code {
    pub(super) fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(super) fn open_history(
        keys: &crate::kernel::keys::Keys,
        id: Id,
        grant_id: grant::Id,
        epoch: i64,
        value: &Sealed,
    ) -> Result<Self, crate::history::Error> {
        let plaintext = keys.open(&value.0, &Binding::new(id, grant_id, epoch))?;
        let mut bytes = Zeroizing::new([0; 32]);
        if plaintext.len() != bytes.len() {
            return Err(crate::history::Error::Integrity(
                "invitation retains a 32-byte original code",
            ));
        }
        bytes.copy_from_slice(&plaintext);
        Ok(Self(bytes))
    }

    pub(super) fn matches(&self, other: &Self) -> bool {
        self.0[..] == other.0[..]
    }
    pub(super) fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = Zeroizing::new([0; 32]);
        getrandom::fill(&mut bytes[..])?;
        Ok(Self(bytes))
    }

    fn text(&self) -> Zeroizing<String> {
        Zeroizing::new(URL_SAFE_NO_PAD.encode(&self.0[..]))
    }

    /// The digest that locates this code under one lookup key version.
    pub(super) fn lookup(&self, key: &keys::Key) -> Result<[u8; 32], cryptography::Error> {
        struct Input<'a>(&'a [u8; 32]);
        impl cryptography::Input<2> for Input<'_> {
            fn fields(&self) -> [&[u8]; 2] {
                [b"nucleus/invitation-lookup/v1", self.0]
            }
        }
        Ok(*key.sign(&Encoded::new(&Input(&self.0))?).as_ref())
    }

    pub(super) fn seal(
        &self,
        keys: &Keys,
        id: Id,
        grant_id: grant::Id,
        epoch: i64,
    ) -> Result<Sealed, keys::Error> {
        struct Input<'a> {
            binding: Binding,
            code: &'a Code,
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
                &self.code.0[..]
            }
        }
        Ok(Sealed(keys.data.seal(&Input {
            binding: Binding::new(id, grant_id, epoch),
            code: self,
        })?))
    }

    pub(super) fn open(
        keys: &Keys,
        id: Id,
        grant_id: grant::Id,
        epoch: i64,
        value: &Sealed,
    ) -> Result<Self, Error> {
        let plaintext = keys
            .data
            .open(&value.0, &Binding::new(id, grant_id, epoch))?;
        let mut bytes = Zeroizing::new([0; 32]);
        if plaintext.len() != bytes.len() {
            return Err(Error::Facts {
                rule: "invitation retains a 32-byte original code",
            });
        }
        bytes.copy_from_slice(&plaintext);
        Ok(Self(bytes))
    }
}

struct Binding([u8; 40]);

impl Binding {
    fn new(id: Id, grant_id: grant::Id, epoch: i64) -> Self {
        let mut bytes = [0; 40];
        bytes[..16].copy_from_slice(id.as_bytes());
        bytes[16..32].copy_from_slice(grant_id.as_bytes());
        bytes[32..].copy_from_slice(&epoch.to_be_bytes());
        Self(bytes)
    }
}

impl envelope::Binding for Binding {
    fn domain(&self) -> &[u8] {
        b"nucleus/invitation/v2"
    }
    fn identity(&self) -> &[u8] {
        &self.0
    }
    fn field(&self) -> &[u8] {
        b"invitation.code"
    }
}
