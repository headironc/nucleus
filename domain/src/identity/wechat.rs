//! The independent initial binding revision exists even while no WeChat identity is bound.

use serde::{Deserialize, Serialize};
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = wechat::Revision)]
pub struct Revision(Uuid);

impl Revision {
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }

    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
}
