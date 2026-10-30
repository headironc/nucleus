//! Permanent avatar advancement facts owned by the account object.

use tokio_postgres::types::{FromSql, ToSql};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromSql, ToSql)]
#[postgres(transparent)]
pub(crate) struct Id(Uuid);

impl Id {
    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}
