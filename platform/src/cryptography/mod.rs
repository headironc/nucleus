//! Cryptographic algorithms, typed encodings, and envelopes for application-owned protocols.

pub mod envelope;
pub mod hmac;
pub mod key;
pub mod sha256;

use derive_more::AsRef;
use thiserror::Error;
use zeroize::Zeroizing;

/// A protocol-owned input with a fixed number and order of fields.
pub trait Input<const N: usize> {
    fn fields(&self) -> [&[u8]; N];
}

/// Length-checked, length-prefixed bytes; field meaning and order belong to the protocol owner.
#[derive(AsRef)]
#[as_ref([u8])]
pub struct Encoded(Zeroizing<Vec<u8>>);

impl Encoded {
    pub fn new<const N: usize>(input: &impl Input<N>) -> Result<Self, Error> {
        let fields = input.fields();
        let mut lengths = [0_u32; N];
        let mut size = 0_usize;
        for (field, length) in fields.iter().zip(&mut lengths) {
            *length = u32::try_from(field.len())?;
            size = size
                .checked_add(4)
                .and_then(|size| size.checked_add(field.len()))
                .ok_or(Error::Size)?;
        }
        let mut encoded = Zeroizing::new(Vec::new());
        encoded.try_reserve_exact(size)?;
        for (field, length) in fields.into_iter().zip(lengths) {
            encoded.extend_from_slice(&length.to_be_bytes());
            encoded.extend_from_slice(field);
        }
        Ok(Self(encoded))
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("cryptographic field exceeds the encoding limit")]
    Length(#[from] std::num::TryFromIntError),
    #[error("cryptographic encoding size overflow")]
    Size,
    #[error("cannot allocate cryptographic encoding")]
    Allocation(#[from] std::collections::TryReserveError),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::cryptography::{Encoded, Input};

    pub(super) struct Fixture<'a>(pub(super) [&'a [u8]; 4]);

    impl Input<4> for Fixture<'_> {
        fn fields(&self) -> [&[u8]; 4] {
            self.0
        }
    }

    #[test]
    fn matches_independent_length_prefixed_vectors() {
        // Python struct.pack('>I').
        let input = Fixture([b"a", b"bc", b"", "界".as_bytes()]);
        let encoded = Encoded::new(&input).expect("bounded encoding");
        assert_eq!(
            encoded.as_ref(),
            &[
                0, 0, 0, 1, 97, 0, 0, 0, 2, 98, 99, 0, 0, 0, 0, 0, 0, 0, 3, 231, 149, 140
            ]
        );
    }
}
