//! AES-256-GCM-SIV and the V1 envelope layout. Callers own business bindings.

use std::collections::TryReserveError;

use aes_gcm_siv::{AeadInOut, Aes256GcmSiv, KeyInit, Tag};
use derive_more::AsRef;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::cryptography::{
    self, Encoded,
    key::{self, Id},
};

const HEADER: usize = 21;
const TAG: usize = 16;

/// Business-owned values; Platform supplies the header and the fixed AAD encoding.
pub trait Binding {
    fn domain(&self) -> &[u8];
    /// Identity bytes in the order fixed by the owning protocol.
    fn identity(&self) -> &[u8];
    fn field(&self) -> &[u8];
}

/// A protocol-owned plaintext and its business binding, constructed together by the owner.
pub trait Input: Binding {
    fn plaintext(&self) -> &[u8];
}

struct Associated<'a, B> {
    header: &'a Header,
    binding: &'a B,
}

impl<B: Binding> cryptography::Input<4> for Associated<'_, B> {
    fn fields(&self) -> [&[u8]; 4] {
        [
            self.binding.domain(),
            self.header.as_ref(),
            self.binding.identity(),
            self.binding.field(),
        ]
    }
}

struct Nonce([u8; 12]);

impl Nonce {
    fn generate(
        fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
    ) -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 12];
        fill(&mut bytes)?;
        Ok(Self(bytes))
    }
}

#[derive(AsRef)]
pub struct Header {
    #[as_ref]
    bytes: [u8; HEADER],
    key_id: Id,
}

impl Header {
    fn new(key_id: Id, nonce: Nonce) -> Self {
        let mut bytes = [0; HEADER];
        bytes[0] = 1;
        bytes[1..9].copy_from_slice(&key_id.as_ref().to_be_bytes());
        bytes[9..].copy_from_slice(&nonce.0);
        Self { bytes, key_id }
    }

    fn parse(bytes: [u8; HEADER]) -> Result<Self, ParseError> {
        if bytes[0] != 1 {
            return Err(ParseError::Version);
        }
        let mut key_id = [0; 8];
        key_id.copy_from_slice(&bytes[1..9]);
        Ok(Self {
            bytes,
            key_id: Id::new(i64::from_be_bytes(key_id))?,
        })
    }

    pub fn key_id(&self) -> Id {
        self.key_id
    }

    fn nonce(&self) -> aes_gcm_siv::Nonce {
        let mut bytes = [0; 12];
        bytes.copy_from_slice(&self.bytes[9..]);
        bytes.into()
    }
}

/// Parsing establishes layout only; authenticity is established by opening with a binding.
#[derive(AsRef)]
pub struct Envelope {
    #[as_ref([u8])]
    bytes: Vec<u8>,
    header: Header,
}

impl Envelope {
    pub fn new(bytes: Vec<u8>) -> Result<Self, ParseError> {
        if bytes.len() < HEADER + TAG {
            return Err(ParseError::Length);
        }
        let mut header = [0; HEADER];
        header.copy_from_slice(&bytes[..HEADER]);
        Ok(Self {
            bytes,
            header: Header::parse(header)?,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }
}

pub struct Key {
    id: Id,
    material: Zeroizing<[u8; 32]>,
}

impl Key {
    pub fn new(id: Id, material: Zeroizing<[u8; 32]>) -> Self {
        Self { id, material }
    }

    pub fn id(&self) -> Id {
        self.id
    }

    pub fn seal(&self, input: &impl Input) -> Result<Envelope, Error> {
        self.seal_with(input, getrandom::fill)
    }

    fn seal_with(
        &self,
        input: &impl Input,
        fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
    ) -> Result<Envelope, Error> {
        let header = Header::new(self.id, Nonce::generate(fill)?);
        let aad = Encoded::new(&Associated {
            header: &header,
            binding: input,
        })?;
        let plaintext = input.plaintext();
        let size = plaintext
            .len()
            .checked_add(HEADER + TAG)
            .ok_or(Error::Size)?;
        let mut buffer = Zeroizing::new(Vec::new());
        buffer.try_reserve_exact(size)?;
        buffer.extend_from_slice(header.as_ref());
        buffer.extend_from_slice(plaintext);
        let cipher = Aes256GcmSiv::new((&*self.material).into());
        let tag = cipher.encrypt_inout_detached(
            &header.nonce(),
            aad.as_ref(),
            buffer[HEADER..].as_mut().into(),
        )?;
        buffer.extend_from_slice(&tag);
        // Only ciphertext leaves the clearing buffer; no reallocation occurred while plaintext lived.
        Ok(Envelope {
            bytes: std::mem::take(&mut *buffer),
            header,
        })
    }

    pub fn open(
        &self,
        envelope: &Envelope,
        binding: &impl Binding,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        let header = envelope.header();
        if header.key_id() != self.id {
            return Err(Error::Key);
        }
        let aad = Encoded::new(&Associated { header, binding })?;
        let end = envelope.bytes.len() - TAG;
        let mut plaintext = Zeroizing::new(Vec::new());
        plaintext.try_reserve_exact(end - HEADER)?;
        plaintext.extend_from_slice(&envelope.bytes[HEADER..end]);
        let mut tag = [0; TAG];
        tag.copy_from_slice(&envelope.bytes[end..]);
        let cipher = Aes256GcmSiv::new((&*self.material).into());
        cipher.decrypt_inout_detached(
            &header.nonce(),
            aad.as_ref(),
            plaintext.as_mut_slice().into(),
            &Tag::from(tag),
        )?;
        Ok(plaintext)
    }
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("truncated cryptographic envelope")]
    Length,
    #[error("unsupported cryptographic envelope version")]
    Version,
    #[error("negative envelope key id")]
    Id(#[from] key::Error),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("envelope key id does not match the selected key")]
    Key,
    #[error("cryptographic envelope size overflow")]
    Size,
    #[error("cannot allocate cryptographic envelope")]
    Allocation(#[from] TryReserveError),
    #[error("cannot obtain cryptographic randomness")]
    Random(#[from] getrandom::Error),
    #[error("cannot encode envelope binding: {0}")]
    Encoding(#[from] cryptography::Error),
    #[error("authenticated encryption failed")]
    Authentication(#[from] aes_gcm_siv::Error),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use zeroize::Zeroizing;

    use crate::cryptography::{
        envelope::{Binding, Envelope, Error, Input, Key, ParseError},
        key::Id,
    };

    struct Record<'a> {
        domain: &'a [u8],
        id: &'a [u8],
        field: &'a [u8],
        plaintext: &'a [u8],
    }

    impl Input for Record<'_> {
        fn plaintext(&self) -> &[u8] {
            self.plaintext
        }
    }

    impl Binding for Record<'_> {
        fn domain(&self) -> &[u8] {
            self.domain
        }
        fn identity(&self) -> &[u8] {
            self.id
        }
        fn field(&self) -> &[u8] {
            self.field
        }
    }

    const ID: [u8; 16] = [1, 137, 15, 110, 123, 128, 112, 0, 128, 0, 0, 0, 0, 0, 0, 0];

    fn record() -> Record<'static> {
        Record {
            domain: b"nucleus/identity/envelope/v1",
            id: &ID,
            field: b"challenge.mobile",
            plaintext: b"13800000000",
        }
    }

    fn key(id: i64) -> Key {
        Key::new(
            Id::new(id).expect("id"),
            Zeroizing::new(std::array::from_fn(|i| u8::try_from(i).expect("32 bytes"))),
        )
    }

    #[test]
    fn matches_independent_envelope_vectors() {
        // Python cryptography AESGCMSIV, key 0..31, nonce 0..11, struct.pack length prefixes.
        for (id, expected) in [
            (
                0,
                "010000000000000000000102030405060708090a0bb089c0edf060fbe8af85dc9060e507e1393773c7dabb6521ad283f",
            ),
            (
                i64::MAX,
                "017fffffffffffffff000102030405060708090a0b710bf6ab0d53c9fbde88b301526bfd2d8490226ee4e912bd0ebb32",
            ),
        ] {
            let envelope = key(id)
                .seal_with(&record(), |nonce| {
                    for (i, byte) in nonce.iter_mut().enumerate() {
                        *byte = u8::try_from(i).expect("12 bytes");
                    }
                    Ok(())
                })
                .expect("seal");
            assert_eq!(hex::encode(envelope.as_ref()), expected);
            assert_eq!(*envelope.header().key_id().as_ref(), id);
            let restored =
                Envelope::new(hex::decode(expected).expect("fixture")).expect("envelope");
            assert_eq!(
                &*key(id).open(&restored, &record()).expect("open"),
                b"13800000000"
            );
        }
    }

    #[test]
    fn authenticates_every_binding_and_envelope_component() {
        let envelope = key(0).seal(&record()).expect("seal");
        for binding in [
            Record {
                domain: b"other-domain",
                ..record()
            },
            Record {
                id: &[0; 16],
                ..record()
            },
            Record {
                field: b"other-field",
                ..record()
            },
        ] {
            assert!(matches!(
                key(0).open(&envelope, &binding),
                Err(Error::Authentication(_))
            ));
        }
        assert!(matches!(
            Key::new(Id::new(0).expect("id"), Zeroizing::new([0; 32])).open(&envelope, &record()),
            Err(Error::Authentication(_))
        ));
        for index in 1..envelope.as_ref().len() {
            let mut bytes = envelope.as_ref().to_vec();
            bytes[index] ^= 1;
            let modified = Envelope::new(bytes).expect("unchanged layout");
            if index < 9 {
                assert!(matches!(key(0).open(&modified, &record()), Err(Error::Key)));
            }
            // Even with the substituted ID selected, the authenticated header must fail.
            assert!(matches!(
                key(*modified.header().key_id().as_ref()).open(&modified, &record()),
                Err(Error::Authentication(_))
            ));
        }
        for size in 0..37 {
            assert!(matches!(
                Envelope::new(envelope.as_ref()[..size].to_vec()),
                Err(ParseError::Length)
            ));
        }
        for size in 37..envelope.as_ref().len() {
            let truncated = Envelope::new(envelope.as_ref()[..size].to_vec()).expect("layout");
            assert!(matches!(
                key(0).open(&truncated, &record()),
                Err(Error::Authentication(_))
            ));
        }
        let mut bytes = envelope.as_ref().to_vec();
        bytes[0] = 2;
        assert!(matches!(Envelope::new(bytes), Err(ParseError::Version)));
        let mut bytes = envelope.as_ref().to_vec();
        bytes[1] = 128;
        assert!(matches!(Envelope::new(bytes), Err(ParseError::Id(_))));
    }

    #[test]
    fn obtains_a_new_nonce_and_never_falls_back_on_random_failure() {
        let mut calls = 0;
        for value in [7, 8] {
            let envelope = key(0)
                .seal_with(
                    &Record {
                        plaintext: b"",
                        ..record()
                    },
                    |nonce| {
                        calls += 1;
                        nonce.fill(value);
                        Ok(())
                    },
                )
                .expect("empty plaintext is valid for the algorithm");
            assert_eq!(&envelope.as_ref()[9..21], &[value; 12]);
            assert!(key(0).open(&envelope, &record()).expect("open").is_empty());
        }
        assert_eq!(calls, 2);
        assert!(matches!(
            key(0).seal_with(&record(), |nonce| {
                nonce.fill(99);
                Err(getrandom::Error::UNSUPPORTED)
            }),
            Err(Error::Random(_))
        ));
    }
}
