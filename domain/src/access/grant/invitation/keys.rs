//! Invitation key sets: sealed original codes and their lookup digests.

use std::collections::BTreeMap;

use platform::cryptography::{Encoded, envelope, hmac};
use zeroize::Zeroizing;

use crate::keys::{Derived, Id};

/// The `encryption` and `lookup` uses this capability consumes.
pub struct Keys {
    pub data: Data,
    pub lookup: Lookup,
}

/// AES-256-GCM-SIV versions sealing the original code of each invitation.
pub struct Data {
    active: envelope::Key,
    retained: BTreeMap<Id, envelope::Key>,
}

impl Data {
    pub(crate) fn new(derived: Derived) -> Self {
        let (active, retained) = derived.split(envelope::Key::new);
        Self { active, retained }
    }

    pub(in crate::access) fn seal(
        &self,
        input: &impl envelope::Input,
    ) -> Result<envelope::Envelope, Error> {
        Ok(self.active.seal(input)?)
    }

    pub(in crate::access) fn open(
        &self,
        value: &envelope::Envelope,
        binding: &impl envelope::Binding,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        let key_id = value.header().key_id();
        let key = if key_id == self.active.id() {
            &self.active
        } else {
            self.retained.get(&key_id).ok_or(Error::Missing(key_id))?
        };
        Ok(key.open(value, binding)?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("missing invitation data key {0:?}")]
    Missing(Id),
    #[error("{0}")]
    Envelope(#[from] envelope::Error),
}

/// HMAC-SHA-256 versions locating a submitted code without storing it.
pub struct Lookup {
    active: Key,
    retained: BTreeMap<Id, Key>,
}

/// A versioned lookup key issued only by the validated Invitation set.
pub struct Key {
    id: Id,
    material: hmac::Key,
}

impl Lookup {
    pub(crate) fn new(derived: Derived) -> Self {
        let (active, retained) = derived.split(|id, material| Key {
            id,
            material: hmac::Key::new(material),
        });
        Self { active, retained }
    }

    pub(crate) fn active(&self) -> &Key {
        &self.active
    }

    pub(in crate::access) fn fetch(&self, key_id: Id) -> Result<&Key, Missing> {
        if key_id == self.active.id() {
            return Ok(&self.active);
        }
        self.retained.get(&key_id).ok_or(Missing(key_id))
    }

    /// A new code must not repeat a digest under any retained version.
    pub(crate) fn all(&self) -> impl Iterator<Item = &Key> {
        std::iter::once(&self.active).chain(self.retained.values())
    }
}

impl Key {
    pub(crate) fn id(&self) -> Id {
        self.id
    }

    pub(in crate::access) fn sign(&self, input: &Encoded) -> hmac::Tag {
        self.material.sign(input)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("missing invitation lookup key {0:?}")]
pub struct Missing(Id);

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use platform::cryptography::envelope;
    use zeroize::Zeroizing;

    use crate::{
        access::grant::invitation::keys::{Data, Keys, Lookup},
        keys::{Derived, Entry, Id, Material},
    };

    fn material(first: u8) -> Material {
        let mut bytes = [0; 32];
        bytes[0] = first;
        Material(Zeroizing::new(bytes))
    }

    #[test]
    fn lookup_selects_active_and_retained_versions_without_fallback() {
        let id = Id::new(0).expect("id");
        let retained = Id::new(1).expect("id");
        let lookup = Lookup::new(Derived::fixed(
            retained,
            vec![
                Entry::new(id, material(1)),
                Entry::new(retained, material(2)),
            ],
        ));
        assert_eq!(lookup.active().id(), retained);
        assert_eq!(lookup.fetch(id).expect("retained").id(), id);
        assert!(lookup.fetch(Id::new(2).expect("id")).is_err());
        assert_eq!(
            lookup.all().map(|key| key.id()).collect::<Vec<_>>(),
            [retained, id]
        );
    }

    struct Input;
    impl envelope::Binding for Input {
        fn domain(&self) -> &[u8] {
            b"nucleus/invitation/v1"
        }
        fn identity(&self) -> &[u8] {
            &[7; 16]
        }
        fn field(&self) -> &[u8] {
            b"invitation.code"
        }
    }
    impl envelope::Input for Input {
        fn plaintext(&self) -> &[u8] {
            b"fixture original invitation code!"
        }
    }

    #[test]
    fn data_opens_retained_versions_and_rejects_missing_keys() {
        let old = Id::new(0).expect("id");
        let current = Data::new(Derived::fixed(old, vec![Entry::new(old, material(1))]));
        let sealed = current.seal(&Input).expect("sealed code");
        let rotated = Data::new(Derived::fixed(
            Id::new(1).expect("id"),
            vec![
                Entry::new(old, material(1)),
                Entry::new(Id::new(1).expect("id"), material(2)),
            ],
        ));
        assert_eq!(
            &rotated.open(&sealed, &Input).expect("retained open")[..],
            b"fixture original invitation code!"
        );
        let missing = Data::new(Derived::fixed(
            Id::new(1).expect("id"),
            vec![Entry::new(Id::new(1).expect("id"), material(2))],
        ));
        assert!(matches!(
            missing.open(&sealed, &Input),
            Err(super::Error::Missing(id)) if id == old
        ));
        let _ = Keys {
            data: rotated,
            lookup: Lookup::new(Derived::fixed(old, vec![Entry::new(old, material(3))])),
        };
    }
}
