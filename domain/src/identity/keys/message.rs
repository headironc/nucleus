//! Versioned code-proof keys, separate from Challenge and mobile keys.

use std::collections::BTreeMap;

use platform::cryptography::{self, hmac, sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::identity::{
    keys::{self, Entry, Id, Material, Purpose},
    message::{self, Code, proof::Proof},
};

pub struct Key {
    id: Id,
    fingerprint: sha256::Digest,
    material: hmac::Key,
}

impl Key {
    fn new(id: Id, material: Zeroizing<[u8; 32]>) -> Result<Self, cryptography::Error> {
        let fingerprint = keys::fingerprint(id, Purpose::Message, &material)?;
        Ok(Self {
            id,
            fingerprint,
            material: hmac::Key::new(material),
        })
    }

    pub fn id(&self) -> Id {
        self.id
    }

    pub fn fingerprint(&self) -> &sha256::Digest {
        &self.fingerprint
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
    pub fn new(active_id: Id, keys: Vec<Entry>) -> Result<Self, keys::Error> {
        keys::validate(active_id, &keys)?;
        let mut retained = BTreeMap::new();
        for Entry {
            id,
            material: Material(material),
        } in keys
        {
            retained.insert(id, Key::new(id, material)?);
        }
        let active = retained
            .remove(&active_id)
            .ok_or(keys::Error::Active(active_id))?;
        Ok(Self { active, retained })
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
