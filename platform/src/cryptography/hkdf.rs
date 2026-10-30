//! HKDF-SHA-256 (RFC 5869) expansion of one 32-byte key.

use ring::hkdf;
use zeroize::Zeroizing;

use crate::cryptography::Error;

struct Length;

impl hkdf::KeyType for Length {
    fn len(&self) -> usize {
        32
    }
}

/// Extracts `material` with `salt` and expands `info` into exactly 32 bytes.
/// The output is cleared on release; ring's internal pseudorandom key has no such guarantee.
pub fn derive(salt: &[u8], material: &[u8; 32], info: &[u8]) -> Result<Zeroizing<[u8; 32]>, Error> {
    let mut output = Zeroizing::new([0; 32]);
    let info = [info];
    match hkdf::Salt::new(hkdf::HKDF_SHA256, salt)
        .extract(material)
        .expand(&info, Length)
    {
        Ok(okm) => match okm.fill(&mut output[..]) {
            Ok(()) => Ok(output),
            Err(_) => Err(Error::Derivation),
        },
        Err(_) => Err(Error::Derivation),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::cryptography::hkdf::derive;

    #[test]
    fn matches_independent_vectors_and_separates_info_and_salt() {
        // Python hmac/hashlib RFC 5869 extract-then-expand, cross-checked with the
        // cryptography package's HKDF; key bytes 0..31.
        let material = std::array::from_fn(|index| u8::try_from(index).expect("32 bytes"));
        for (info, expected) in [
            (
                "encryption",
                "e274e356668ac4e10092ed98b843d18e7c16126479dcdbec9039e914bfcde61b",
            ),
            (
                "lookup",
                "1ed7ac6e6e3b7f3330eb9434260c6e15037d9663bce2bba06dc220e9bf83a84a",
            ),
        ] {
            let derived = derive(b"nucleus/key/v1", &material, info.as_bytes()).expect("derive");
            assert_eq!(hex::encode(&derived[..]), expected);
        }
        let derived = derive(b"nucleus/key/v1", &material, b"seal").expect("derive");
        assert_ne!(
            &derived[..],
            &derive(b"nucleus/key/v2", &material, b"seal").expect("derive")[..]
        );
        assert_ne!(
            &derived[..],
            &derive(b"nucleus/key/v1", &[0; 32], b"seal").expect("derive")[..]
        );
    }
}
