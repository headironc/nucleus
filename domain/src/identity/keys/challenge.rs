//! `lookup` key versions for Challenge token proofs and occupancy.

use std::collections::BTreeMap;

use platform::cryptography::{self, hmac};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    identity::challenge::{Token, proof::Proof},
    keys::{Derived, Id},
};

/// A versioned key issued only by the validated Challenge key set.
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

    pub fn prove(&self, token: &Token) -> Result<Proof, cryptography::Error> {
        Proof::new(&self.material, token)
    }

    pub fn verify(&self, token: &Token, proof: &Proof) -> Result<bool, cryptography::Error> {
        proof.verify(&self.material, token)
    }
}

/// The active key and every retained version, immutable after loading.
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

    /// All versions must participate in a new token-occupancy lookup.
    pub fn all(&self) -> impl Iterator<Item = &Key> {
        std::iter::once(&self.active).chain(self.retained.values())
    }
}

#[derive(Debug, Error)]
#[error("missing challenge key {0:?}")]
pub struct Missing(Id);

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use zeroize::Zeroizing;

    use crate::{
        identity::keys::challenge::Keys,
        keys::{Derived, Entry, Id, Material},
    };

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
    fn selects_active_and_retained_keys_without_fallback() {
        let id = Id::new(0).expect("id");
        let old = Keys::new(Derived::fixed(id, fixture(2)));
        let token = "fictional-token".parse().expect("token");
        let proof = old.active().prove(&token).expect("proof");
        let new = Keys::new(Derived::fixed(Id::new(1).expect("id"), fixture(2)));
        assert!(!new.active().verify(&token, &proof).expect("verify"));
        assert!(
            new.fetch(id)
                .expect("retained")
                .verify(&token, &proof)
                .expect("verify")
        );
        assert!(new.fetch(Id::new(2).expect("id")).is_err());
        assert_eq!(new.all().count(), 2);
    }
}
