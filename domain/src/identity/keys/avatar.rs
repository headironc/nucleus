//! Versioned `link` keys for signed avatar links.

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

    pub fn find(&self, id: Id) -> Option<&Key> {
        if id == self.active.id {
            return Some(&self.active);
        }
        self.retained.get(&id)
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
