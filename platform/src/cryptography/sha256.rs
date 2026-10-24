//! SHA-256 digests of length-prefixed inputs.

use std::fmt;

use bytes::BytesMut;
use derive_more::AsRef;
use ring::digest;
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};

use crate::cryptography::Encoded;

/// A SHA-256 digest, distinct from an HMAC authentication tag.
#[derive(Clone, PartialEq, Eq, AsRef)]
pub struct Digest([u8; 32]);

impl Digest {
    pub fn new(input: &Encoded) -> Self {
        let mut bytes = [0; 32];
        bytes.copy_from_slice(digest::digest(&digest::SHA256, input.as_ref()).as_ref());
        Self(bytes)
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Digest(..)")
    }
}

impl<'a> FromSql<'a> for Digest {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Self(<&[u8]>::from_sql(kind, raw)?.try_into()?))
    }

    fn accepts(kind: &Type) -> bool {
        <&[u8] as FromSql>::accepts(kind)
    }
}

impl ToSql for Digest {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        self.0.as_slice().to_sql(kind, out)
    }

    fn accepts(kind: &Type) -> bool {
        <&[u8] as ToSql>::accepts(kind)
    }

    to_sql_checked!();
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use bytes::BytesMut;
    use tokio_postgres::types::{FromSql, ToSql, Type};

    use crate::cryptography::{Encoded, sha256::Digest, tests::Fixture};

    #[test]
    fn decodes_only_32_byte_digests() {
        let bytes = [42; 32];
        let digest = Digest::from_sql(&Type::BYTEA, &bytes).expect("digest");
        assert_eq!(digest.as_ref(), &bytes);
        let mut output = BytesMut::new();
        digest
            .to_sql_checked(&Type::BYTEA, &mut output)
            .expect("encode bytea");
        assert_eq!(&output[..], &bytes);
        assert!(
            digest
                .to_sql_checked(&Type::TEXT, &mut BytesMut::new())
                .is_err()
        );
        assert_eq!(format!("{digest:?}"), "Digest(..)");
        for bytes in [&[][..], &[42; 31][..], &[42; 33][..]] {
            let error = Digest::from_sql(&Type::BYTEA, bytes).expect_err("invalid length");
            assert!(
                error
                    .downcast_ref::<std::array::TryFromSliceError>()
                    .is_some()
            );
        }
        assert!(Digest::from_sql_null(&Type::BYTEA).is_err());
        assert!(<Digest as FromSql>::accepts(&Type::BYTEA));
        assert!(!<Digest as FromSql>::accepts(&Type::TEXT));
        assert!(!<Digest as FromSql>::accepts(&Type::INT8));
    }

    #[test]
    fn matches_independent_digest_vector() {
        // Python hashlib.sha256 over struct.pack('>I') length prefixes.
        let input = Fixture([b"a", b"bc", b"", "界".as_bytes()]);
        let encoded = Encoded::new(&input).expect("encoding");
        assert_eq!(
            Digest::new(&encoded).as_ref(),
            &[
                55, 193, 215, 159, 98, 227, 240, 186, 162, 247, 21, 2, 210, 248, 213, 254, 209,
                127, 99, 233, 230, 1, 118, 66, 143, 37, 66, 205, 218, 140, 22, 239
            ]
        );
    }
}
