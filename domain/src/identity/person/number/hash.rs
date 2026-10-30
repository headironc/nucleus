//! The resident identity-number digest and its fixed input encoding.

use std::fmt;

use bytes::BytesMut;
use derive_more::AsRef;
use platform::cryptography::{self, Encoded, hmac};
use tokio_postgres::types::{FromSql, IsNull, Kind, ToSql, Type, to_sql_checked};

use crate::identity::person::number::IdNumber;

#[derive(AsRef, PartialEq, Eq)]
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
            Kind::Domain(inner) => inner == &Type::BYTEA,
            _ => kind == &Type::BYTEA,
        }
    }

    pub(crate) fn new(key: &hmac::Key, number: &IdNumber) -> Result<Self, cryptography::Error> {
        struct Input<'a>(&'a IdNumber);
        impl cryptography::Input<2> for Input<'_> {
            fn fields(&self) -> [&[u8]; 2] {
                [b"nucleus/identity/id_number/v1", self.0.as_ref().as_bytes()]
            }
        }
        Ok(Self(*key.sign(&Encoded::new(&Input(number))?).as_ref()))
    }
}
