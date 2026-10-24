//! Versioned `proof` keys for message code proofs.

use std::collections::BTreeMap;

use platform::cryptography::{self, hmac};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    identity::message::{self, Code, proof::Proof},
    keys::{Derived, Id},
};

pub struct Key {
    id: Id,
    material: hmac::Key,
}

impl Key {
    fn new(id: Id, material: Zeroizing<[u8; 32]>) -> Self {
        Self {
            id,
            material: hmac::Key::new(material),
        }
    }

    pub fn id(&self) -> Id {
        self.id
    }

    pub fn prove(
        &self,
        message_id: message::Id,
        code: &Code,
    ) -> Result<Proof, cryptography::Error> {
        Proof::new(&self.material, message_id, code)
    }

    pub fn verify(
        &self,
        message_id: message::Id,
        code: &Code,
        proof: &Proof,
    ) -> Result<bool, cryptography::Error> {
        proof.verify(&self.material, message_id, code)
    }
}

pub struct Keys {
    active: Key,
    retained: BTreeMap<Id, Key>,
}

impl Keys {
    pub(crate) fn new(derived: Derived) -> Self {
        let (active, retained) = derived.split(Key::new);
        Self { active, retained }
    }

    pub fn active(&self) -> &Key {
        &self.active
    }

    pub fn fetch(&self, key_id: Id) -> Result<&Key, Missing> {
        if key_id == self.active.id() {
            return Ok(&self.active);
        }
        self.retained.get(&key_id).ok_or(Missing(key_id))
    }

    pub fn all(&self) -> impl Iterator<Item = &Key> {
        std::iter::once(&self.active).chain(self.retained.values())
    }
}

#[derive(Debug, Error)]
#[error("missing message key {0:?}")]
pub struct Missing(Id);
