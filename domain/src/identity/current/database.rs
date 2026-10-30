//! Current identity reads authenticate each fact through its owner in one snapshot.

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    history::{self, Read, Verified},
    identity::{
        account, avatar,
        current::{Account, Actor, Error},
        keys::Keys,
        person::{self, revision},
        session::{self, state::State as Session},
        website::AppId,
    },
    kernel::App,
};

pub(super) async fn lock_account(client: &Client, id: account::Id) -> Result<(), Error> {
    account::state::lock_shared(client, id).await?;
    Ok(())
}

pub(super) async fn lock_accounts(
    client: &Client,
    actor: account::Id,
    target: account::Id,
) -> Result<(), Error> {
    account::state::lock_management(client, actor, target).await?;
    Ok(())
}

pub(crate) struct State {
    pub(super) session: Verified<Session>,
    pub(super) account: Verified<account::state::State>,
    pub(super) person_id: person::Id,
    pub(super) revision_id: revision::Id,
    pub(super) observed_at: DateTime<Utc>,
}

impl State {
    pub(crate) fn person_id(&self) -> person::Id {
        self.person_id
    }

    pub(crate) fn account_id(&self) -> account::Id {
        self.account.id()
    }

    pub(crate) fn revision_id(&self) -> revision::Id {
        self.revision_id
    }

    pub(crate) fn account_state(&self) -> &Verified<account::state::State> {
        &self.account
    }

    pub(crate) fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }

    pub(super) fn validate(&self, app_id: &AppId) -> Result<(), Error> {
        if self.session.app_id() != app_id {
            return Err(Error::Facts {
                rule: "session application is the configured website",
            });
        }
        if !self.session.available(self.observed_at)
            || self
                .account
                .current()
                .is_none_or(|account| account.suspension_id().is_some())
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

pub(super) async fn find_session_by_id(
    client: &Client,
    app: &App,
    read: Read,
    id: session::Id,
) -> Result<Option<State>, Error> {
    let now = app.context().now(client).await?;
    find_at(client, app, read, id, now).await
}

/// The executor already obtained this decisive time after taking the object locks.
pub(super) async fn find_at(
    client: &Client,
    app: &App,
    read: Read,
    id: session::Id,
    now: DateTime<Utc>,
) -> Result<Option<State>, Error> {
    let Some(session) =
        history::find::<Session>(client, app.keys(), app.anchors(), read, id, now).await?
    else {
        return Ok(None);
    };
    let account = history::find::<account::state::State>(
        client,
        app.keys(),
        app.anchors(),
        read,
        session.account_id(),
        now,
    )
    .await?
    .ok_or(Error::Facts {
        rule: "a retained session has its account state",
    })?;
    let person_id = account.person_id();
    let person = person::state::find(client, app.keys(), app.anchors(), read, person_id, now)
        .await?
        .ok_or(Error::Facts {
            rule: "an account retains its permanent person",
        })?;
    Ok(Some(State {
        session,
        account,
        person_id,
        revision_id: person.revision_id(),
        observed_at: now,
    }))
}

pub(super) async fn find_person_by_actor(
    client: &Client,
    app: &App,
    read: Read,
    actor: &Actor,
) -> Result<Option<State>, Error> {
    find_session_by_id(client, app, read, actor.session_id).await
}

/// Called only after the original proof or actor and current session have been confirmed.
pub(super) async fn profile(
    client: &Client,
    state: &State,
    keys: &Keys,
    history: history::Keys<'_>,
) -> Result<revision::Profile, Error> {
    let profile = revision::fetch(client, history, state.revision_id).await?;
    let current = state.account.current().ok_or(Error::Invalid)?;
    if profile.person_id() != state.person_id
        || keys.number.hash(profile.id_number())?.as_ref() != current.number().as_ref()
    {
        return Err(Error::Facts {
            rule: "the current identity number matches its sealed occupancy",
        });
    }
    Ok(profile)
}

pub(super) async fn deliver(
    client: &Client,
    state: &State,
    keys: &Keys,
    app: &App,
    avatar: &avatar::Service,
) -> Result<Account, Error> {
    let profile = profile(client, state, keys, app.keys()).await?;
    let (_, name, id_number) = profile.into_values();
    let snapshot = avatar
        .find_by_account(client, &state.account)
        .await?
        .ok_or(Error::Facts {
            rule: "a current account has its current avatar",
        })?;
    let now = app.context().now(client).await?;
    Ok(Account {
        id: state.account.id(),
        person_id: state.person_id,
        revision_id: state.revision_id,
        name,
        suffix: id_number.suffix().to_owned(),
        avatar: avatar.sign(snapshot, now)?,
    })
}
