//! Versioned `proof` keys for canonical original inputs.

use std::collections::BTreeMap;

use platform::cryptography::{Encoded, hmac};

use crate::keys::{Derived, Id};

pub struct Keys {
    active: Key,
    retained: BTreeMap<Id, Key>,
}

pub struct Key {
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
    pub fn active(&self) -> &Key {
        &self.active
    }
    pub fn fetch(&self, id: Id) -> Result<&Key, Missing> {
        if id == self.active.id {
            return Ok(&self.active);
        }
        self.retained.get(&id).ok_or(Missing(id))
    }
    pub fn all(&self) -> impl Iterator<Item = &Key> {
        std::iter::once(&self.active).chain(self.retained.values())
    }
}

impl Key {
    pub fn id(&self) -> Id {
        self.id
    }
    pub(crate) fn sign(&self, input: &Encoded) -> hmac::Tag {
        self.material.sign(input)
    }
    pub(crate) fn verify(&self, input: &Encoded, expected: &[u8; 32]) -> bool {
        self.material.verify(input, expected)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("missing original input key {0:?}")]
pub struct Missing(Id);

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use platform::cryptography::Encoded;
    use zeroize::Zeroizing;

    use crate::{
        identity::keys::request::Keys,
        keys::{Derived, Entry, Id, Material},
    };

    #[test]
    fn verifies_with_the_retained_original_key() {
        let id = Id::new(1).expect("id");
        let entry = || {
            Entry::new(
                id,
                Material(Zeroizing::new(std::array::from_fn(|index| {
                    u8::try_from(index).expect("byte")
                }))),
            )
        };
        let original = Keys::new(Derived::fixed(id, vec![entry()]));
        struct Input;
        impl platform::cryptography::Input<1> for Input {
            fn fields(&self) -> [&[u8]; 1] {
                [b"original input"]
            }
        }
        let input = Encoded::new(&Input).expect("encoding");
        let tag = original.active().sign(&input);
        let active = Id::new(2).expect("new id");
        let rotated = Keys::new(Derived::fixed(
            active,
            vec![
                entry(),
                Entry::new(active, Material(Zeroizing::new([99; 32]))),
            ],
        ));
        assert!(
            rotated
                .fetch(id)
                .expect("retained")
                .verify(&input, tag.as_ref())
        );
        assert!(!rotated.active().verify(&input, tag.as_ref()));
        assert!(rotated.fetch(Id::new(3).expect("absent")).is_err());
    }
}
