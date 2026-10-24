//! Deterministic mobile hashing under the single active mobile key.

use std::collections::BTreeMap;

use platform::cryptography::{self, hmac, sha256};

use crate::identity::{
    keys::{self, Entry, Id, Material, Purpose},
    mobile::{Mobile, hash::Hash},
};

pub struct Keys {
    active: Key,
    retained: BTreeMap<Id, Key>,
}

struct Key {
    id: Id,
    material: hmac::Key,
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
            let fingerprint = keys::fingerprint(id, Purpose::Mobile, &material)?;
            versions.insert(
                id,
                Key {
                    id,
                    material: hmac::Key::new(material),
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
        self.active.id
    }

    /// Metadata for startup and maintenance; retained keys never participate in live hashing.
    pub fn fingerprints(&self) -> impl Iterator<Item = (Id, &sha256::Digest)> {
        std::iter::once(&self.active)
            .chain(self.retained.values())
            .map(|key| (key.id, &key.fingerprint))
    }

    pub fn hash(&self, mobile: &Mobile) -> Result<Hash, cryptography::Error> {
        Hash::new(&self.active.material, mobile)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use zeroize::Zeroizing;

    use crate::identity::keys::{Entry, Id, Material, mobile::Keys};

    fn entries() -> Vec<Entry> {
        vec![
            Entry::new(
                Id::new(0).expect("id"),
                Material(Zeroizing::new(std::array::from_fn(|i| {
                    u8::try_from(i).expect("32 bytes")
                }))),
            ),
            Entry::new(Id::new(1).expect("id"), Material(Zeroizing::new([42; 32]))),
        ]
    }

    #[test]
    fn matches_independent_hash_and_fingerprint_vectors() {
        // Python hmac.digest and hashlib.sha256 over separately encoded fields.
        let keys = Keys::new(Id::new(0).expect("id"), entries()).expect("keys");
        let mobile = "13800000000".parse().expect("mobile");
        assert_eq!(
            hex::encode(keys.hash(&mobile).expect("hash").as_ref()),
            "3845b46ad75387acdf91db39dbe9295ea6ee7abb99b1f77dfbad19a350d3eae6"
        );
        let (_, fingerprint) = keys
            .fingerprints()
            .find(|(id, _)| *id.as_ref() == 0)
            .expect("version");
        assert_eq!(
            hex::encode(fingerprint.as_ref()),
            "3ff6d437fa0c83a950c5d77cc5bb00c2c1be856ff4203dc297de7eb1d80bb605"
        );
        assert_ne!(
            keys.hash(&mobile).expect("hash").as_ref(),
            keys.hash(&"13900000000".parse().expect("mobile"))
                .expect("hash")
                .as_ref()
        );
    }

    #[test]
    fn only_uses_the_active_key() {
        let mobile = "13800000000".parse().expect("mobile");
        let old = Keys::new(Id::new(0).expect("id"), entries()).expect("keys");
        let changed = Keys::new(Id::new(1).expect("id"), entries()).expect("keys");
        let active_only =
            Keys::new(Id::new(1).expect("id"), vec![entries().remove(1)]).expect("keys");
        assert_ne!(
            old.hash(&mobile).expect("hash").as_ref(),
            changed.hash(&mobile).expect("hash").as_ref()
        );
        assert_eq!(
            changed.hash(&mobile).expect("hash").as_ref(),
            active_only.hash(&mobile).expect("hash").as_ref()
        );
        assert_eq!(changed.fingerprints().count(), 2);
        assert_eq!(*changed.active_id().as_ref(), 1);
        assert!(Keys::new(Id::new(2).expect("id"), entries()).is_err());
    }
}
