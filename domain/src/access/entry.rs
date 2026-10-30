//! The authority explicitly selected for Workforce management.

use bytes::BytesMut;
use serde::Deserialize;
use strum::{AsRefStr, EnumString};
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema, AsRefStr, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[schema(as = access::Entry)]
pub enum Entry {
    Domain,
    Institution,
}

impl Entry {
    pub(super) fn can_assign(
        self,
        manager: &crate::access::role::Role,
        selected: &crate::access::role::Role,
    ) -> bool {
        match self {
            Self::Institution => true,
            Self::Domain => {
                selected.level() < manager.level()
                    && selected
                        .permissions()
                        .iter()
                        .all(|permission| manager.permissions().contains(permission))
            }
        }
    }
}

impl<'a> FromSql<'a> for Entry {
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

impl ToSql for Entry {
    fn to_sql(
        &self,
        kind: &Type,
        output: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().to_sql(kind, output)
    }

    fn accepts(kind: &Type) -> bool {
        <&str as ToSql>::accepts(kind)
    }

    to_sql_checked!();
}
