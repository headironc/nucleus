//! HMAC-SHA-256 over length-prefixed inputs.

use derive_more::AsRef;
use ring::hmac;
use zeroize::Zeroizing;

use crate::cryptography::Encoded;

/// Raw material is cleared on release; ring's internal copies have no such guarantee.
pub struct Key(Zeroizing<[u8; 32]>);

impl Key {
    pub fn new(material: Zeroizing<[u8; 32]>) -> Self {
        Self(material)
    }

    pub fn sign(&self, input: &Encoded) -> Tag {
        let key = hmac::Key::new(hmac::HMAC_SHA256, &self.0[..]);
        let mut bytes = [0; 32];
        bytes.copy_from_slice(hmac::sign(&key, input.as_ref()).as_ref());
        Tag(bytes)
    }

    pub fn verify(&self, input: &Encoded, tag: &[u8; 32]) -> bool {
        let key = hmac::Key::new(hmac::HMAC_SHA256, &self.0[..]);
        hmac::verify(&key, input.as_ref(), tag).is_ok()
    }
}

/// A computed HMAC-SHA-256 tag, constructed only by signing.
#[derive(AsRef)]
pub struct Tag([u8; 32]);

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use zeroize::Zeroizing;

    use crate::cryptography::{Encoded, hmac::Key, tests::Fixture};

    #[test]
    fn matches_independent_vector_and_rejects_wrong_inputs() {
        // Python struct.pack('>I') and hmac.digest; key bytes 0..31.
        let input = Encoded::new(&Fixture([b"a", b"bc", b"", "界".as_bytes()])).expect("encoding");
        let material = std::array::from_fn(|index| u8::try_from(index).expect("32 byte key"));
        let key = Key::new(Zeroizing::new(material));
        let expected = [
            109, 36, 108, 45, 44, 228, 158, 82, 5, 239, 23, 102, 237, 232, 53, 43, 67, 86, 107, 9,
            27, 55, 152, 88, 106, 159, 39, 39, 127, 99, 80, 129,
        ];
        let tag = key.sign(&input);
        assert_eq!(tag.as_ref(), &expected);
        assert!(key.verify(&input, tag.as_ref()));
        let different = Encoded::new(&Fixture([b"ab", b"c", b"", "界".as_bytes()]))
            .expect("different boundaries");
        assert!(!key.verify(&different, tag.as_ref()));
        assert!(!Key::new(Zeroizing::new([0; 32])).verify(&input, tag.as_ref()));
        let mut changed = expected;
        changed[0] ^= 1;
        assert!(!key.verify(&input, &changed));
    }
}
