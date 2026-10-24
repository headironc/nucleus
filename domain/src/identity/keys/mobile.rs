//! Deterministic mobile hashing under the single active `lookup` key.

use platform::cryptography::{self, hmac};

use crate::{
    identity::mobile::{Mobile, hash::Hash},
    keys::Derived,
};

/// Lookups never try retained versions; those exist only in the root metadata.
pub struct Keys {
    active: hmac::Key,
}

impl Keys {
    pub(crate) fn new(derived: Derived) -> Self {
        let (active, _) = derived.split(|_, material| hmac::Key::new(material));
        Self { active }
    }

    pub fn hash(&self, mobile: &Mobile) -> Result<Hash, cryptography::Error> {
        Hash::new(&self.active, mobile)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use zeroize::Zeroizing;

    use crate::{
        identity::keys::mobile::Keys,
        keys::{Derived, Entry, Id, Material},
    };

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
    fn matches_independent_hash_vectors() {
        // Python hmac.digest over separately encoded fields with a fixed lookup key 0..31.
        let keys = Keys::new(Derived::fixed(Id::new(0).expect("id"), entries()));
        let mobile = "13800000000".parse().expect("mobile");
        assert_eq!(
            hex::encode(keys.hash(&mobile).expect("hash").as_ref()),
            "3845b46ad75387acdf91db39dbe9295ea6ee7abb99b1f77dfbad19a350d3eae6"
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
        let old = Keys::new(Derived::fixed(Id::new(0).expect("id"), entries()));
        let changed = Keys::new(Derived::fixed(Id::new(1).expect("id"), entries()));
        let active_only = Keys::new(Derived::fixed(
            Id::new(1).expect("id"),
            vec![entries().remove(1)],
        ));
        assert_ne!(
            old.hash(&mobile).expect("hash").as_ref(),
            changed.hash(&mobile).expect("hash").as_ref()
        );
        assert_eq!(
            changed.hash(&mobile).expect("hash").as_ref(),
            active_only.hash(&mobile).expect("hash").as_ref()
        );
    }
}
