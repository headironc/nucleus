//! Permanent account suspension results and their authenticated history.

pub mod history;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    history::{Error as HistoryError, Verified},
    identity::{
        account::record::{Event, Record},
        person::revision,
    },
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, FromSql, ToSql, ToSchema,
)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = suspension::Id)]
pub struct Id(Uuid);

impl Id {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = suspension::Suspension)]
pub struct Suspension {
    id: Id,
    #[schema(value_type = String, format = DateTime)]
    suspended_at: DateTime<Utc>,
}

impl Suspension {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn suspended_at(&self) -> DateTime<Utc> {
        self.suspended_at
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = suspension::Resumption)]
pub struct Resumption {
    id: Id,
    #[schema(value_type = String, format = DateTime)]
    resumed_at: DateTime<Utc>,
}

impl Resumption {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn resumed_at(&self) -> DateTime<Utc> {
        self.resumed_at
    }
}

pub(crate) enum Receipt {
    Suspension(Suspension),
    Resumption(Resumption),
}

impl Receipt {
    pub(crate) fn read(record: &Verified<Record>) -> Result<Self, HistoryError> {
        match record.event() {
            Event::Suspension { id, .. } => Ok(Self::Suspension(Suspension {
                id: *id,
                suspended_at: record.happened_at(),
            })),
            Event::Resumption { id, .. } => Ok(Self::Resumption(Resumption {
                id: *id,
                resumed_at: record.happened_at(),
            })),
            _ => Err(HistoryError::Integrity(
                "account management returns its declared event",
            )),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("account state fact check failed: {rule}")]
    Facts { rule: &'static str },
    #[error("{0}")]
    History(#[from] HistoryError),
    #[error("historical account state identity is invalid: {0}")]
    Revision(#[from] revision::ReadError),
}

impl Error {
    pub(crate) fn unreliable(&self) -> bool {
        match self {
            Self::Facts { .. } => true,
            Self::History(error) => error.corrupt(),
            Self::Revision(error) => error.unreliable(),
        }
    }
}
