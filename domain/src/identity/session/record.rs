//! Persistent Session invariants independent of short-lived handoff material.

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    history::{self, Read, Verified},
    identity::{
        account, authentication, keys, operation,
        registration::attempt,
        session::{Error, Id, Token, state},
        website::AppId,
    },
    kernel::App,
};

pub(crate) struct Session {
    state: Verified<state::State>,
}

impl Session {
    pub(crate) fn id(&self) -> Id {
        self.state.id()
    }
    pub(crate) fn account_id(&self) -> account::Id {
        self.state.account_id()
    }
    pub(crate) fn app_id(&self) -> &AppId {
        self.state.app_id()
    }
    pub(super) fn authentication_id(&self) -> authentication::Id {
        self.state.authentication_id()
    }
    pub(crate) fn receipt_id(&self) -> Option<attempt::Id> {
        self.state.receipt_id()
    }
    pub(crate) fn expires_at(&self) -> DateTime<Utc> {
        self.state.expires_at()
    }
    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        self.state.created_at()
    }
    pub(crate) fn matches(&self, token: &Token) -> bool {
        token.0.matches(self.state.hash())
    }
    pub(crate) fn validate(&self, app_id: &AppId, now: DateTime<Utc>) -> Result<(), Error> {
        if self.app_id() != app_id {
            return Err(Error::Facts {
                rule: "session application is the configured website",
            });
        }
        self.validate_time(now)
    }
    pub(super) fn validate_time(&self, now: DateTime<Utc>) -> Result<(), Error> {
        if self.created_at() > now {
            return Err(Error::Facts {
                rule: "session creation precedes trusted time",
            });
        }
        Ok(())
    }

    pub(crate) async fn available(
        &self,
        client: &Client,
        app: &App,
        now: DateTime<Utc>,
    ) -> Result<bool, Error> {
        let account = history::find::<account::state::State>(
            client,
            app.keys(),
            app.anchors(),
            Read::Shared,
            self.account_id(),
            now,
        )
        .await?
        .ok_or(Error::Facts {
            rule: "retained session belongs to its permanent account",
        })?;
        Ok(self.state.available(now)
            && account
                .current()
                .is_some_and(|current| current.suspension_id().is_none()))
    }

    pub(crate) fn retained(&self, now: DateTime<Utc>) -> Result<bool, Error> {
        let ended = self
            .state
            .revoked_at()
            .map_or(self.expires_at(), |revoked| revoked.min(self.expires_at()));
        let until = ended
            .checked_add_signed(chrono::TimeDelta::hours(24))
            .ok_or(Error::Time)?;
        Ok(now < until)
    }

    pub(crate) async fn deletion(
        &self,
        client: &Client,
        keys: &keys::credential::Keys,
        now: DateTime<Utc>,
    ) -> Result<Option<Deletion>, Error> {
        let handoff =
            super::handoff::expired(client, self, keys, now, self.state.revoked_at().is_some())
                .await?;
        Ok(if !self.retained(now)? {
            Some(Deletion::Session(self.id()))
        } else if handoff {
            Some(Deletion::Handoff(self.id()))
        } else {
            None
        })
    }
}

pub(crate) async fn find_by_authentication(
    client: &Client,
    app: &App,
    read: Read,
    authentication_id: authentication::Id,
    now: DateTime<Utc>,
) -> Result<Option<Session>, Error> {
    Ok(state::located(
        client,
        app.keys(),
        app.anchors(),
        read,
        state::Location::Authentication(authentication_id),
        now,
    )
    .await?
    .map(|state| Session { state }))
}

pub(crate) async fn find_by_receipt(
    client: &Client,
    app: &App,
    read: Read,
    receipt_id: attempt::Id,
    now: DateTime<Utc>,
) -> Result<Option<Session>, Error> {
    Ok(state::located(
        client,
        app.keys(),
        app.anchors(),
        read,
        state::Location::Receipt(receipt_id),
        now,
    )
    .await?
    .map(|state| Session { state }))
}

/// A cleanup owner chooses eligibility under its original-operation guard.
pub(crate) enum Deletion {
    Handoff(Id),
    Session(Id),
}

impl Deletion {
    pub(crate) async fn lock(&self, client: &Client) -> Result<(), Error> {
        let (Self::Handoff(id) | Self::Session(id)) = self;
        state::lock_existing(client, *id).await?;
        Ok(())
    }

    pub(crate) async fn remove(
        &self,
        client: &Client,
        app: &App,
        operation: operation::Id,
        now: DateTime<Utc>,
        pending: &mut Option<history::Pending>,
    ) -> Result<(), Error> {
        match self {
            Self::Handoff(id) => {
                client
                    .execute("DELETE FROM identity.handoffs WHERE id = $1", &[id])
                    .await?;
            }
            Self::Session(id) => {
                let original = history::find::<state::State>(
                    client,
                    app.keys(),
                    app.anchors(),
                    Read::Locked,
                    *id,
                    now,
                )
                .await?
                .ok_or(Error::Facts {
                    rule: "the selected session remains locked until removal",
                })?;
                let change = history::Draft::remove(&original, operation)
                    .write(client, app.anchors())
                    .await?;
                match pending {
                    Some(pending) => pending.extend(change)?,
                    slot @ None => *slot = Some(change),
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn remains(
        &self,
        client: &Client,
        app: &App,
        now: DateTime<Utc>,
    ) -> Result<bool, Error> {
        let (Self::Handoff(id) | Self::Session(id)) = self;
        let session = history::find::<state::State>(
            client,
            app.keys(),
            app.anchors(),
            Read::Locked,
            *id,
            now,
        )
        .await?;
        if session.is_none() {
            state::removed(client, *id).await?;
            return Ok(false);
        }
        match self {
            Self::Session(_) => Ok(true),
            Self::Handoff(_) => Ok(client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM identity.handoffs WHERE id=$1)",
                    &[id],
                )
                .await?
                .try_get(0)?),
        }
    }
}
