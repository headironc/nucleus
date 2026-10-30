//! The configured identity of the website receiving SMS sessions.

use std::str::FromStr;

use bytes::BytesMut;
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppId(String);

impl FromStr for AppId {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        if value.is_empty()
            || value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(Error);
        }
        Ok(Self(value.to_owned()))
    }
}

impl AsRef<str> for AppId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<'a> FromSql<'a> for AppId {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(<&str>::from_sql(kind, raw)?.parse()?)
    }
    fn accepts(kind: &Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

impl ToSql for AppId {
    fn to_sql(
        &self,
        kind: &Type,
        output: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.0.to_sql(kind, output)
    }
    fn accepts(kind: &Type) -> bool {
        <String as ToSql>::accepts(kind)
    }
    to_sql_checked!();
}

#[derive(Debug, thiserror::Error)]
#[error("website application identity must be nonempty without whitespace or control characters")]
pub struct Error;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::identity::website::AppId;

    #[test]
    fn preserves_accepted_values_and_rejects_empty_whitespace_and_controls() {
        for value in ["website-fixture", "网站", "WX-Case"] {
            assert_eq!(value.parse::<AppId>().expect("identity").as_ref(), value);
        }
        for value in ["", " app", "app\n", "a\u{2003}b", "a\0b"] {
            assert!(value.parse::<AppId>().is_err());
        }
    }
}
