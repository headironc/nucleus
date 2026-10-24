//! Challenge proof key versions. Files and environment belong to application assembly.

use std::collections::BTreeMap;

use platform::cryptography::{self, hmac, sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::identity::{
    challenge::{Token, proof::Proof},
    keys::{self, Entry, Id, Material, Purpose},
};

/// A versioned key issued only by the validated Challenge key set.
pub struct Key {
    id: Id,
    fingerprint: sha256::Digest,
    material: hmac::Key,
}

impl Key {
    fn new(id: Id, material: Zeroizing<[u8; 32]>) -> Result<Self, cryptography::Error> {
        let fingerprint = keys::fingerprint(id, Purpose::Challenge, &material)?;
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

    use crate::identity::keys::{
        Entry, Id, Material,
        challenge::{Key, Keys},
    };

    #[test]
    fn matches_independent_fingerprint_vectors() {
        // Python hashlib over big-endian length prefixes and key bytes 0..31.
        let material = std::array::from_fn(|index| u8::try_from(index).expect("32 bytes"));
        for (id, expected) in [
            (
                0,
                [
                    44, 13, 251, 149, 176, 190, 83, 135, 253, 93, 4, 69, 60, 110, 94, 120, 50, 166,
                    116, 228, 99, 249, 26, 22, 162, 130, 71, 231, 121, 97, 69, 127,
                ],
            ),
            (
                i64::MAX,
                [
                    215, 99, 201, 94, 92, 215, 118, 235, 30, 14, 120, 49, 186, 19, 232, 27, 58, 53,
                    31, 185, 13, 69, 84, 7, 246, 26, 197, 250, 254, 156, 3, 152,
                ],
            ),
        ] {
            let key = Key::new(Id::new(id).expect("id"), Zeroizing::new(material)).expect("key");
            assert_eq!(key.fingerprint().as_ref(), &expected);
        }
    }

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
        let old = Keys::new(id, fixture(2)).expect("keys");
        let token = "fictional-token".parse().expect("token");
        let proof = old.active().prove(&token).expect("proof");
        let new = Keys::new(Id::new(1).expect("id"), fixture(2)).expect("keys");
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
