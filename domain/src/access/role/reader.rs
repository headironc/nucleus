//! Authenticated role loading shared by current authorization and definition consumers.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};

use crate::{
    access::{
        capability::Scope,
        role::{Error, Id, Role, state::State},
    },
    history::{self, Read, Verified, View},
    kernel::App,
};

#[derive(Clone)]
pub(crate) struct Reader {
    pub(super) app: App,
    view: Option<View>,
}

impl Reader {
    pub(crate) fn new(app: App) -> Self {
        Self { app, view: None }
    }

    /// Bind every role read in a repeatable-read transaction to its pre-BEGIN view.
    pub(crate) fn at(&self, view: View) -> Self {
        Self {
            app: self.app.clone(),
            view: Some(view),
        }
    }

    pub(crate) fn anchors(&self) -> &history::Anchors {
        self.app.anchors()
    }

    pub(crate) fn keys(&self) -> history::Keys<'_> {
        self.app.keys()
    }

    pub(in crate::access) async fn holders(
        &self,
        client: &Client,
        now: DateTime<Utc>,
    ) -> Result<Vec<Verified<crate::access::holder::State>>, Error> {
        Ok(history::all(client, self.keys(), self.anchors(), self.read()?, now).await?)
    }

    pub(in crate::access) async fn holder(
        &self,
        client: &Client,
        id: crate::identity::account::Id,
        now: DateTime<Utc>,
    ) -> Result<Option<Verified<crate::access::holder::State>>, Error> {
        Ok(history::find(
            client,
            self.app.keys(),
            self.app.anchors(),
            self.read()?,
            id,
            now,
        )
        .await?)
    }

    pub(crate) fn read(&self) -> Result<Read, history::Error> {
        Ok(Read::Snapshot(match self.view {
            Some(view) => view,
            None => self.app.anchors().view()?,
        }))
    }

    pub(super) fn state(
        &self,
        row: &Row,
        read: Read,
        now: DateTime<Utc>,
    ) -> Result<Verified<State>, Error> {
        Ok(history::state(
            row,
            self.app.keys(),
            self.app.anchors(),
            read,
            now,
        )?)
    }

    pub(super) async fn find_state(
        &self,
        client: &Client,
        id: Id,
        now: DateTime<Utc>,
    ) -> Result<Option<Verified<State>>, Error> {
        Ok(history::find(
            client,
            self.app.keys(),
            self.app.anchors(),
            self.read()?,
            id,
            now,
        )
        .await?)
    }

    pub(crate) async fn find(
        &self,
        client: &Client,
        id: Id,
        scope: Scope,
        now: DateTime<Utc>,
    ) -> Result<Option<Role>, Error> {
        Ok(self
            .find_state(client, id, now)
            .await?
            .filter(|state| state.active() && state.scope() == scope)
            .map(|state| state.role().clone()))
    }

    pub(super) async fn all(
        &self,
        client: &Client,
        now: DateTime<Utc>,
    ) -> Result<Vec<Verified<State>>, Error> {
        let states: Vec<Verified<State>> = history::all(
            client,
            self.app.keys(),
            self.app.anchors(),
            self.read()?,
            now,
        )
        .await?;
        super::state::check_collection(&states)?;
        Ok(states)
    }
}
