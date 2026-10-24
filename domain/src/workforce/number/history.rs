//! Identifiers of permanent number allocation and correction facts.

use serde::Serialize;
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = number::history::Id)]
pub struct Id(Uuid);

impl Id {
    pub(super) fn from_record(id: Uuid) -> Self {
        Self(id)
    }
}
