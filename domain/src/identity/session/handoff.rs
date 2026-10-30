//! Short-lived original token delivery, independent of ordinary Session use.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};

use crate::{
    database::sealed::Sealed,
    identity::{
        credential, keys,
        session::{Error, Id, Session, Token},
        website::AppId,
    },
};

struct Handoff {
    token: Sealed,
    expires_at: DateTime<Utc>,
}

impl TryFrom<Row> for Handoff {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            token: row.try_get("token")?,
            expires_at: row.try_get("expires_at")?,
        })
    }
}

impl Handoff {
    fn open(&self, keys: &keys::credential::Keys, session: &Session) -> Result<Token, Error> {
        if self.expires_at <= session.created_at() || self.expires_at > session.expires_at() {
            return Err(Error::Facts {
                rule: "session preserves its authentication source and credential lifetime",
            });
        }
        let token = credential::Secret::open(
            keys,
            &self.token.0,
            credential::Binding::Handoff(session.id()),
        )?;
        let token = Token(token);
        if !session.matches(&token) {
            return Err(Error::Facts {
                rule: "session digest matches its original handoff token",
            });
        }
        Ok(token)
    }
}

async fn find_by_session(client: &Client, id: Id) -> Result<Option<Handoff>, Error> {
    client
        .query_opt(
            "SELECT token, expires_at FROM identity.handoffs WHERE id = $1",
            &[&id],
        )
        .await?
        .map(Handoff::try_from)
        .transpose()
}

pub(crate) async fn remove(client: &Client, id: Id) -> Result<(), crate::database::Query> {
    client
        .execute("DELETE FROM identity.handoffs WHERE id=$1", &[&id])
        .await?;
    Ok(())
}

pub(crate) async fn deliver(
    client: &Client,
    session: &Session,
    keys: &keys::credential::Keys,
    app_id: &AppId,
    now: DateTime<Utc>,
) -> Result<Token, Error> {
    session.validate(app_id, now)?;
    let handoff = find_by_session(client, session.id())
        .await?
        .ok_or(Error::Unavailable)?;
    let token = handoff.open(keys, session)?;
    if session.expires_at() <= now || handoff.expires_at <= now {
        return Err(Error::Unavailable);
    }
    Ok(token)
}

pub(super) async fn verify(
    client: &Client,
    session: &Session,
    keys: &keys::credential::Keys,
) -> Result<(), Error> {
    if let Some(handoff) = find_by_session(client, session.id()).await? {
        handoff.open(keys, session)?;
    }
    Ok(())
}

pub(super) async fn expired(
    client: &Client,
    session: &Session,
    keys: &keys::credential::Keys,
    now: DateTime<Utc>,
    revoked: bool,
) -> Result<bool, Error> {
    let Some(handoff) = find_by_session(client, session.id()).await? else {
        return Ok(false);
    };
    handoff.open(keys, session)?;
    Ok(revoked || handoff.expires_at <= now)
}
