//! Permanent person identity and stable real-name values.

pub mod name;
pub mod number;
pub(crate) mod record;
pub mod revision;
pub(crate) mod state;
pub(crate) mod transition;

#[cfg(test)]
mod tests;

use std::{str::FromStr, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    history::{self, Read, Verified},
    identity::{account, keys},
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, FromSql, ToSql, ToSchema,
)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = person::Id)]
pub struct Id(Uuid);

impl std::fmt::Display for Id {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for Id {
    type Err = ParseIdError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(input)?))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid UUID text")]
pub struct ParseIdError(#[from] uuid::Error);

impl Id {
    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

async fn profile(
    client: &impl tokio_postgres::GenericClient,
    history: history::Keys<'_>,
    number: &keys::number::Keys,
    id: Id,
    account: Option<&Verified<account::state::State>>,
    state: Option<&Verified<state::State>>,
) -> Result<Option<revision::Profile>, ReadError> {
    let Some(state) = state else {
        if account.is_some() {
            return Err(ReadError::Facts {
                rule: "an authenticated account retains its permanent person",
            });
        }
        return Ok(None);
    };
    if state.id() != id {
        return Err(ReadError::Facts {
            rule: "the loaded person state has its requested identity",
        });
    }
    let revision_id = state.revision_id();
    let profile = revision::fetch(client, history, revision_id).await?;
    if profile.person_id() != id {
        return Err(revision::Error::Person.into());
    }
    if let Some(account) = account {
        if account.person_id() != id {
            return Err(ReadError::Facts {
                rule: "the authenticated account belongs to the requested person",
            });
        }
        if let Some(current) = account.current()
            && current.number() != &number.hash(profile.id_number())?
        {
            return Err(ReadError::Facts {
                rule: "current account identity-number hash",
            });
        }
    }
    Ok(Some(profile))
}

#[derive(Clone)]
pub(crate) struct Reader {
    keys: Arc<keys::Keys>,
    accounts: account::Reader,
    read: Read,
}

impl Reader {
    pub(crate) fn new(accounts: account::Reader, keys: Arc<keys::Keys>) -> Self {
        Self {
            keys,
            accounts,
            read: Read::Locked,
        }
    }
    pub(crate) fn with_read(&self, read: Read) -> Self {
        Self {
            keys: Arc::clone(&self.keys),
            accounts: self.accounts.with_read(read),
            read,
        }
    }
    pub(crate) async fn state(
        &self,
        client: &tokio_postgres::Client,
        id: Id,
        now: DateTime<Utc>,
    ) -> Result<Option<Verified<state::State>>, ReadError> {
        Ok(state::find(
            client,
            self.accounts.history_keys(),
            self.accounts.anchors(),
            self.read,
            id,
            now,
        )
        .await?)
    }
    pub(crate) async fn profile(
        &self,
        client: &tokio_postgres::Client,
        id: Id,
        now: DateTime<Utc>,
    ) -> Result<Option<revision::Profile>, ReadError> {
        let account = self.accounts.person_state(client, id, now).await?;
        let state = self.state(client, id, now).await?;
        profile(
            client,
            self.accounts.history_keys(),
            &self.keys.number,
            id,
            account.as_ref(),
            state.as_ref(),
        )
        .await
    }
    pub(crate) async fn find_identity(
        &self,
        client: &tokio_postgres::Client,
        id: Id,
        now: DateTime<Utc>,
    ) -> Result<Option<revision::Identity>, ReadError> {
        Ok(self
            .profile(client, id, now)
            .await?
            .map(revision::Profile::into_identity))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("{0}")]
    History(#[from] history::Error),
    #[error("{0}")]
    Account(#[from] account::ReadError),
    #[error("current person query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("current person decoding failed: {0}")]
    Decode(#[source] crate::database::Query),
    #[error("current person integrity failed: {rule}")]
    Facts { rule: &'static str },
    #[error("{0}")]
    Revision(#[from] revision::Error),
    #[error("current person identity-number digest computation failed")]
    Crypto(#[from] platform::cryptography::Error),
}

impl ReadError {
    pub(crate) fn unreliable(&self) -> bool {
        match self {
            Self::History(error) => error.corrupt(),
            Self::Account(error) => error.unreliable(),
            Self::Revision(error) => error.unreliable(),
            Self::Decode(_) | Self::Facts { .. } => true,
            _ => false,
        }
    }
}

impl From<tokio_postgres::Error> for ReadError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(error.into())
    }
}

/// Authenticate every retained Person state and its contiguous record family without installing
/// anchors. Startup uses the same inspection before [`history::initialize`].
#[cfg(test)]
pub(crate) async fn verify(
    client: &tokio_postgres::Client,
    keys: history::Keys<'_>,
    now: DateTime<Utc>,
) -> Result<(), history::Error> {
    history::inspect::<state::State>(client, keys, now)
        .await
        .map(|_| ())
}
