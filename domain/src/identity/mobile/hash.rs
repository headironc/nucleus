//! The Identity mobile digest and its fixed input encoding.

use std::fmt;

use bytes::BytesMut;
use derive_more::AsRef;
use platform::cryptography::{self, Encoded, hmac};
use tokio_postgres::types::{FromSql, IsNull, Kind, ToSql, Type, to_sql_checked};

use crate::identity::mobile::Mobile;

#[derive(AsRef)]
#[as_ref([u8; 32])]
pub struct Hash([u8; 32]);

impl fmt::Debug for Hash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Hash(..)")
    }
}

impl<'a> FromSql<'a> for Hash {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(<&[u8]>::from_sql(kind, raw)?.try_into()?))
    }

    fn accepts(kind: &Type) -> bool {
        Hash::accepts_sql(kind)
    }
}

impl ToSql for Hash {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.0.as_slice().to_sql(kind, out)
    }

    fn accepts(kind: &Type) -> bool {
        Hash::accepts_sql(kind)
    }

    to_sql_checked!();
}

impl Hash {
    fn accepts_sql(kind: &Type) -> bool {
        match kind.kind() {
            Kind::Domain(r#type) => r#type == &Type::BYTEA,
            _ => kind == &Type::BYTEA,
        }
    }

    pub(crate) fn new(key: &hmac::Key, mobile: &Mobile) -> Result<Self, cryptography::Error> {
        Ok(Self(*key.sign(&Encoded::new(&Input(mobile))?).as_ref()))
    }
}

struct Input<'a>(&'a Mobile);

impl cryptography::Input<2> for Input<'_> {
    fn fields(&self) -> [&[u8]; 2] {
        [b"nucleus/identity/mobile/v1", self.0.as_bytes()]
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use bytes::BytesMut;
    use tokio_postgres::types::{FromSql, Kind, ToSql, Type};

    use crate::identity::mobile::hash::Hash;

    #[test]
    fn accepts_bytea_domains_and_checks_decoded_length() {
        for kind in [
            Type::BYTEA,
            Type::new(
                "digest".into(),
                12345,
                Kind::Domain(Type::BYTEA),
                "identity".into(),
            ),
            Type::new(
                "digest".into(),
                12346,
                Kind::Domain(Type::BYTEA),
                "other".into(),
            ),
            Type::new(
                "payload".into(),
                12347,
                Kind::Domain(Type::BYTEA),
                "other".into(),
            ),
        ] {
            assert!(<Hash as FromSql>::accepts(&kind));
            let hash = Hash::from_sql(&kind, &[42; 32]).expect("stored hash");
            let mut output = BytesMut::new();
            hash.to_sql_checked(&kind, &mut output)
                .expect("compatible parameter");
            assert_eq!(&output[..], &[42; 32]);
            for size in [0, 31, 33] {
                assert!(Hash::from_sql(&kind, &vec![42; size]).is_err());
            }
            assert!(Hash::from_sql_nullable(&kind, None).is_err());
            assert_eq!(format!("{hash:?}"), "Hash(..)");
        }
        let hash = Hash::from_sql(&Type::BYTEA, &[42; 32]).expect("stored hash");
        for kind in [
            Type::UUID,
            Type::TEXT,
            Type::new(
                "digest".into(),
                12348,
                Kind::Domain(Type::TEXT),
                "identity".into(),
            ),
        ] {
            assert!(!<Hash as FromSql>::accepts(&kind));
            assert!(hash.to_sql_checked(&kind, &mut BytesMut::new()).is_err());
        }
    }
}
