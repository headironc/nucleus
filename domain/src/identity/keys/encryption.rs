//! Keys permitted to protect personal data. Record owners define the binding.

use std::collections::BTreeMap;

use platform::cryptography::{envelope, sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::identity::keys::{self, Entry, Id, Material, Purpose};

pub struct Keys {
    active: Key,
    retained: BTreeMap<Id, Key>,
}

struct Key {
    material: envelope::Key,
    fingerprint: sha256::Digest,
}

impl Keys {
    pub fn new(active_id: Id, entries: Vec<Entry>) -> Result<Self, keys::Error> {
        keys::validate(active_id, &entries)?;
        let mut versions = BTreeMap::new();
        for Entry {
            id,
            material: Material(material),
        } in entries
        {
            let fingerprint = keys::fingerprint(id, Purpose::Encryption, &material)?;
            versions.insert(
                id,
                Key {
                    material: envelope::Key::new(id, material),
                    fingerprint,
                },
            );
        }
        let active = versions
            .remove(&active_id)
            .ok_or(keys::Error::Active(active_id))?;
        Ok(Self {
            active,
            retained: versions,
        })
    }

    pub fn active_id(&self) -> Id {
        self.active.material.id()
    }

    pub fn fingerprints(&self) -> impl Iterator<Item = (Id, &sha256::Digest)> {
        std::iter::once(&self.active)
            .chain(self.retained.values())
            .map(|key| (key.material.id(), &key.fingerprint))
    }

    fn fetch(&self, key_id: Id) -> Result<&Key, Error> {
        if key_id == self.active_id() {
            return Ok(&self.active);
        }
        self.retained.get(&key_id).ok_or(Error::Missing(key_id))
    }

    pub(crate) fn seal(&self, input: &impl envelope::Input) -> Result<envelope::Envelope, Error> {
        Ok(self.active.material.seal(input)?)
    }

    pub(crate) fn open(
        &self,
        value: &envelope::Envelope,
        binding: &impl envelope::Binding,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        Ok(self
            .fetch(value.header().key_id())?
            .material
            .open(value, binding)?)
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("missing personal encryption key {0:?}")]
    Missing(Id),
    #[error("{0}")]
    Envelope(#[from] envelope::Error),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use zeroize::Zeroizing;

    use crate::identity::keys::{Entry, Id, Material, encryption::Keys};

    #[test]
    fn matches_independent_purpose_fingerprint() {
        let id = Id::new(0).expect("id");
        let keys = Keys::new(
            id,
            vec![Entry::new(
                id,
                Material(Zeroizing::new(std::array::from_fn(|i| {
                    u8::try_from(i).expect("32 bytes")
                }))),
            )],
        )
        .expect("keys");
        let (actual, fingerprint) = keys.fingerprints().next().expect("active");
        assert_eq!(actual, id);
        assert_eq!(
            hex::encode(fingerprint.as_ref()),
            "ffdf8d2cefab43caf296e02f2485737f21ca38467bd9eec32c8d99cb18c3852c"
        );
        assert!(keys.fetch(Id::new(1).expect("id")).is_err());
        assert!(Keys::new(id, vec![]).is_err());
    }
}
