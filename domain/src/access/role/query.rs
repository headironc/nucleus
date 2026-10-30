//! The executor binds authorization and role results to one authenticated snapshot.

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    access::{
        capability::Scope,
        grant,
        role::{Code, CommandError, List, Rejection, Role, Service, change, database},
    },
    history::View,
    identity::current::Actor,
    kernel::Query,
    pagination::Pagination,
};

impl Service {
    pub async fn read_current(&self, actor: &Actor) -> Result<Role, CommandError> {
        self.app
            .query(&Current {
                service: self,
                actor,
            })
            .await
    }

    pub async fn list(&self, actor: &Actor, pagination: Pagination) -> Result<List, CommandError> {
        self.fetch_list(actor, pagination, Scope::Institution).await
    }

    pub(super) async fn fetch_list(
        &self,
        actor: &Actor,
        pagination: Pagination,
        scope: Scope,
    ) -> Result<List, CommandError> {
        self.app
            .query(&Definitions {
                service: self,
                actor,
                pagination,
                scope,
            })
            .await
    }

    pub async fn changes(
        &self,
        actor: &Actor,
        filter: change::Filter,
    ) -> Result<change::List, CommandError> {
        self.fetch_changes(actor, filter, Scope::Institution).await
    }

    pub(super) async fn fetch_changes(
        &self,
        actor: &Actor,
        filter: change::Filter,
        scope: Scope,
    ) -> Result<change::List, CommandError> {
        self.app
            .query(&History {
                service: self,
                actor,
                filter,
                scope,
            })
            .await
    }
}

struct Current<'a> {
    service: &'a Service,
    actor: &'a Actor,
}

impl Query for Current<'_> {
    type Authorized = Role;
    type Output = Role;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<Role, CommandError> {
        let roles = self.service.roles.at(view);
        let snapshot =
            database::find_by_actor(client, &roles, &self.service.current, self.actor).await?;
        let now = snapshot.state.observed_at();
        Ok(snapshot.deliver(&roles, now)?)
    }

    async fn read(&self, _: &Client, role: Role, _: View) -> Result<Role, CommandError> {
        Ok(role)
    }
}

async fn authorize(
    service: &Service,
    client: &Client,
    actor: &Actor,
    scope: Scope,
    definitions: bool,
    view: View,
) -> Result<DateTime<Utc>, CommandError> {
    let roles = service.roles.at(view);
    let snapshot = database::find_by_actor(client, &roles, &service.current, actor).await?;
    let now = snapshot.state.observed_at();
    let person_id = snapshot.person_id()?;
    let role = snapshot.deliver(&roles, now)?;
    if scope == Scope::Institution {
        return if role.permissions().contains(&Code::InstitutionRoles) {
            Ok(now)
        } else {
            Err(CommandError::Denied)
        };
    }
    if definitions && role.permissions().contains(&Code::InstitutionWorkforce) {
        return Ok(now);
    }
    grant::member::find_by_person(client, &roles, person_id, now)
        .await?
        .ok_or(CommandError::Denied)?
        .authorize(Code::WorkforceRoles)?;
    Ok(now)
}

struct Definitions<'a> {
    service: &'a Service,
    actor: &'a Actor,
    pagination: Pagination,
    scope: Scope,
}

impl Query for Definitions<'_> {
    type Authorized = DateTime<Utc>;
    type Output = List;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<DateTime<Utc>, CommandError> {
        authorize(self.service, client, self.actor, self.scope, true, view).await
    }

    async fn read(
        &self,
        client: &Client,
        now: DateTime<Utc>,
        view: View,
    ) -> Result<List, CommandError> {
        Ok(database::fetch_list(
            client,
            &self.service.roles.at(view),
            self.pagination,
            self.scope,
            now,
        )
        .await?)
    }
}

struct History<'a> {
    service: &'a Service,
    actor: &'a Actor,
    filter: change::Filter,
    scope: Scope,
}

impl Query for History<'_> {
    type Authorized = DateTime<Utc>;
    type Output = change::List;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<DateTime<Utc>, CommandError> {
        authorize(self.service, client, self.actor, self.scope, false, view).await
    }

    async fn read(
        &self,
        client: &Client,
        now: DateTime<Utc>,
        view: View,
    ) -> Result<change::List, CommandError> {
        Ok(change::fetch_list(
            client,
            &self.service.roles.at(view),
            &self.filter,
            self.scope,
            now,
            &self.service.revisions,
        )
        .await?)
    }
}
