use tokio_postgres::types::{FromSql, ToSql};
use uuid::Uuid;

/// The original execution identity of a message dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromSql, ToSql)]
#[postgres(transparent)]
pub(crate) struct Id(Uuid);

impl Id {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}
