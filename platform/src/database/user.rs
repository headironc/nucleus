//! Validated database login names, without role existence or permission claims.

use std::fmt;

use bytes::BytesMut;
use derive_more::AsRef;
use thiserror::Error;
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};

#[derive(Clone, AsRef)]
#[as_ref(str)]
pub struct User(String);

impl User {
    pub fn new(value: String) -> Result<Self, Error> {
        if value.is_empty() || value.contains('\0') {
            return Err(Error);
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for User {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("User(..)")
    }
}

impl ToSql for User {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        self.as_ref().to_sql(kind, out)
    }

    fn accepts(kind: &Type) -> bool {
        <&str as ToSql>::accepts(kind)
    }

    to_sql_checked!();
}

impl<'a> FromSql<'a> for User {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Self::new(String::from_sql(kind, raw)?)?)
    }

    fn accepts(kind: &Type) -> bool {
        <String as FromSql>::accepts(kind)
    }
}

#[derive(Debug, Error)]
#[error("database user must be nonempty without NUL characters")]
pub struct Error;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use bytes::BytesMut;
    use tokio_postgres::types::{IsNull, ToSql, Type};

    use crate::database::user::User;

    #[test]
    fn preserves_login_names_at_the_sql_boundary() {
        for value in ["", "invalid\0user"] {
            assert!(User::new(value.into()).is_err());
        }
        for value in ["fixture", "Case Sensitive User", r#"owner"'角色"#] {
            let user = User::new(value.into()).expect("valid login name");
            assert_eq!(user.as_ref(), value);
            let mut bytes = BytesMut::new();
            assert!(matches!(
                user.to_sql_checked(&Type::TEXT, &mut bytes)
                    .expect("text parameter"),
                IsNull::No
            ));
            assert_eq!(bytes.as_ref(), value.as_bytes());
            assert!(
                user.to_sql_checked(&Type::INT8, &mut BytesMut::new())
                    .is_err()
            );
            assert_eq!(format!("{user:?}"), "User(..)");
        }
    }
}
