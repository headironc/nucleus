//! Active-only resident identity-number hashing under the `lookup` use.

use platform::cryptography::{self, hmac};

use crate::{
    identity::person::number::{IdNumber, hash::Hash},
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

    pub fn hash(&self, number: &IdNumber) -> Result<Hash, cryptography::Error> {
        Hash::new(&self.active, number)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use zeroize::Zeroizing;

    use crate::{
        identity::{keys::number::Keys, person::number::IdNumber},
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
    fn matches_independent_python_vectors_and_uses_only_the_active_key() {
        let number: IdNumber = "000000200002290010"
            .parse()
            .expect("fictitious valid number");
        // Fixed lookup keys: bytes 0..31 for version 0 and 42 repeated for version 1.
        for (id, hash) in [
            (
                0,
                "84842089aec90bea74ef23d9a7c1b4d268957880f506d677e90ca274541b56ef",
            ),
            (
                1,
                "222e65de424595d09690a50e80290c173412f9799584b16c3516d96896787438",
            ),
        ] {
            let keys = Keys::new(Derived::fixed(Id::new(id).expect("id"), entries()));
            assert_eq!(
                hex::encode(keys.hash(&number).expect("hash").as_ref()),
                hash
            );
        }
        let retained = Keys::new(Derived::fixed(Id::new(1).expect("id"), entries()));
        let active = Keys::new(Derived::fixed(
            Id::new(1).expect("id"),
            vec![entries().remove(1)],
        ));
        assert_eq!(
            retained.hash(&number).expect("hash"),
            active.hash(&number).expect("hash")
        );
        let lower: IdNumber = "00000020000229007x".parse().expect("lowercase");
        let upper: IdNumber = "00000020000229007X".parse().expect("uppercase");
        assert_eq!(
            active.hash(&lower).expect("hash"),
            active.hash(&upper).expect("hash")
        );
    }
}
