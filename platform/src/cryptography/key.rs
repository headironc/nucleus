//! Nonnegative numeric versions used by cryptographic envelopes and key files.

use std::str::FromStr;

use bytes::BytesMut;
use derive_more::AsRef;
use serde::Deserialize;
use thiserror::Error;
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, AsRef, Deserialize)]
#[serde(try_from = "String")]
pub struct Id(i64);

impl Id {
    pub fn new(id: i64) -> Result<Self, Error> {
        if id < 0 {
            return Err(Error);
        }
        Ok(Self(id))
    }
}

impl FromStr for Id {
    type Err = ParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let id: i64 = value.parse()?;
        if id.to_string() != value {
            return Err(ParseError::Canonical);
        }
        Ok(Self::new(id)?)
    }
}

impl TryFrom<String> for Id {
    type Error = ParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl<'a> FromSql<'a> for Id {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Self::new(i64::from_sql(kind, raw)?)?)
    }

    fn accepts(kind: &Type) -> bool {
        <i64 as FromSql>::accepts(kind)
    }
}

impl ToSql for Id {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        self.0.to_sql(kind, out)
    }

    fn accepts(kind: &Type) -> bool {
        <i64 as ToSql>::accepts(kind)
    }

    to_sql_checked!();
}

#[derive(Debug, Error)]
#[error("key id must be a nonnegative signed 64-bit integer")]
pub struct Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("key id is not a signed 64-bit integer")]
    Number(#[from] std::num::ParseIntError),
    #[error("key id must be nonnegative")]
    Value(#[from] Error),
    #[error("key id must use canonical decimal text")]
    Canonical,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use bytes::BytesMut;
    use tokio_postgres::types::{FromSql, ToSql, Type};

    use crate::cryptography::key::{Error, Id};

    #[test]
    fn deserializes_only_canonical_nonnegative_versions() {
        for value in [0_i64, i64::MAX] {
            let json = serde_json::to_string(&value.to_string()).expect("decimal JSON string");
            let id: Id = serde_json::from_str(&json).expect("canonical key id");
            assert_eq!(*id.as_ref(), value);
        }
        for value in [
            "",
            "-1",
            "-0",
            "+7",
            "07",
            " 7",
            "7 ",
            "9223372036854775808",
        ] {
            assert!(value.parse::<Id>().is_err());
            let json = serde_json::to_string(value).expect("JSON string");
            assert!(serde_json::from_str::<Id>(&json).is_err());
        }
        for json in ["null", "7", "true", "[]", "{}"] {
            assert!(serde_json::from_str::<Id>(json).is_err());
        }
    }

    #[test]
    fn decodes_only_valid_bigint_versions() {
        for value in [0_i64, i64::MAX] {
            let id = Id::from_sql(&Type::INT8, &value.to_be_bytes()).expect("valid bigint");
            assert_eq!(*id.as_ref(), value);
            let mut output = BytesMut::new();
            id.to_sql_checked(&Type::INT8, &mut output)
                .expect("encode bigint");
            assert_eq!(&output[..], &value.to_be_bytes());
            assert!(
                id.to_sql_checked(&Type::INT4, &mut BytesMut::new())
                    .is_err()
            );
        }
        for value in [-1_i64, i64::MIN] {
            let error = Id::from_sql(&Type::INT8, &value.to_be_bytes()).expect_err("negative");
            assert!(error.downcast_ref::<Error>().is_some());
        }
        assert!(Id::from_sql(&Type::INT8, &[0; 7]).is_err());
        assert!(Id::from_sql_null(&Type::INT8).is_err());
        assert!(<Id as FromSql>::accepts(&Type::INT8));
        assert!(!<Id as FromSql>::accepts(&Type::INT4));
        assert!(!<Id as FromSql>::accepts(&Type::TEXT));
    }

    #[test]
    fn accepts_only_nonnegative_signed_versions() {
        assert!(Id::new(-1).is_err());
        assert!(Id::new(i64::MIN).is_err());
        assert_eq!(*Id::new(0).expect("zero").as_ref(), 0);
        assert_eq!(*Id::new(i64::MAX).expect("maximum").as_ref(), i64::MAX);
    }
}
