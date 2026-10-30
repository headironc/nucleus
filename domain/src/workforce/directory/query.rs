//! Directory reads: permission and result share one read-only snapshot.

use tokio_postgres::Client;

use crate::{
    access::{grant::member, role},
    history::View,
    identity::current::{self, Actor},
    kernel::Query,
    workforce::directory::{
        Collection, Error, Item, Rejection, database, department, roster, rule, selection,
    },
};

/// A current Workforce member who must hold `workforce.directory`.
pub(super) struct Manager<'a> {
    pub(super) actor: &'a Actor,
    pub(super) current: &'a current::Service,
    pub(super) roles: &'a role::Reader,
}

impl Manager<'_> {
    async fn authorize(&self, client: &Client, view: View) -> Result<(), Error> {
        member::authorize(
            client,
            &self.roles.at(view),
            self.current,
            self.actor,
            role::Code::WorkforceDirectory,
        )
        .await?;
        Ok(())
    }
}

/// One complete management collection, including disabled items.
pub(super) struct List<'a> {
    pub(super) manager: Manager<'a>,
    pub(super) collection: Collection,
}

impl Query for List<'_> {
    type Authorized = ();
    type Output = Vec<Item>;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<(), Error> {
        self.manager.authorize(client, view).await
    }

    async fn read(&self, client: &Client, (): (), _: View) -> Result<Vec<Item>, Error> {
        database::fetch_list(client, self.collection).await
    }
}

/// The currently selectable directory for any current, unsuspended member.
pub(super) struct Selection<'a> {
    pub(super) actor: &'a Actor,
    pub(super) current: &'a current::Service,
    pub(super) roles: &'a role::Reader,
}

impl Query for Selection<'_> {
    type Authorized = ();
    type Output = selection::Selection;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<(), Error> {
        member::authorize_participation(client, &self.roles.at(view), self.current, self.actor)
            .await?;
        Ok(())
    }

    async fn read(&self, client: &Client, (): (), _: View) -> Result<selection::Selection, Error> {
        database::fetch_selection(client).await
    }
}

/// Every configured rule and the explicitly missing subtypes.
pub(super) struct Rules<'a> {
    pub(super) manager: Manager<'a>,
}

impl Query for Rules<'_> {
    type Authorized = ();
    type Output = rule::Configuration;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<(), Error> {
        self.manager.authorize(client, view).await
    }

    async fn read(&self, client: &Client, (): (), _: View) -> Result<rule::Configuration, Error> {
        database::fetch_rule_configuration(client).await
    }
}

/// One real department's complete position list.
pub(super) struct Roster<'a> {
    pub(super) manager: Manager<'a>,
    pub(super) department_id: department::Id,
}

impl Query for Roster<'_> {
    type Authorized = ();
    type Output = Vec<roster::Entry>;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<(), Error> {
        self.manager.authorize(client, view).await
    }

    async fn read(&self, client: &Client, (): (), _: View) -> Result<Vec<roster::Entry>, Error> {
        if database::find_department(client, self.department_id)
            .await?
            .is_none()
        {
            return Err(Rejection::Absent.into());
        }
        database::fetch_roster(client, self.department_id).await
    }
}
