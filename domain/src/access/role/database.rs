//! Fixed role queries. Current definitions come only from authenticated state.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, GenericClient, Row};

use crate::{
    access::{
        capability::Scope,
        holder,
        role::{Error, Id, List, Reader, Role, change, state},
    },
    history::{self, Kind as _, Read},
    identity::{
        account,
        current::{self, Actor},
        person::{self, revision},
    },
    pagination::Pagination,
};

pub(in crate::access) struct Snapshot {
    pub(in crate::access) state: current::database::State,
    holder: Option<Row>,
    role: Option<Row>,
    read: Read,
}

pub(in crate::access) async fn empty(client: &impl GenericClient) -> Result<bool, Error> {
    Ok(client
        .query_one("SELECT NOT EXISTS (SELECT 1 FROM access.roles)", &[])
        .await?
        .try_get(0)?)
}

/// Retained references are checked without re-deciding historical role selections.
pub(in crate::access) async fn verify_links(
    client: &Client,
    app: &crate::kernel::App,
    read: Read,
    accounts_empty: bool,
    now: DateTime<Utc>,
) -> Result<(), crate::access::Error> {
    let states = history::all::<state::State>(client, app.keys(), app.anchors(), read, now).await?;
    if !(accounts_empty && states.is_empty())
        && super::Builtin::ALL.iter().any(|system| {
            !states.iter().any(|state| {
                state.active()
                    && state.scope() == system.scope()
                    && state.role().level().0 == system.level()
            })
        })
    {
        return Err(crate::access::Error::Facts { rule: "roles" });
    }
    Ok(())
}

impl Snapshot {
    pub(super) fn person_id(&self) -> Result<person::Id, Error> {
        Ok(self.state.person_id())
    }

    pub(in crate::access) fn authorize(
        self,
        roles: &Reader,
        now: DateTime<Utc>,
        permission: super::Code,
    ) -> Result<(Role, person::Id, revision::Id), Error> {
        let person_id = self.person_id()?;
        let revision_id = self.state.revision_id();
        let role = self.deliver(roles, now)?;
        if !role.permissions().contains(&permission) {
            return Err(Error::Denied);
        }
        Ok((role, person_id, revision_id))
    }

    pub(super) fn deliver(self, roles: &Reader, now: DateTime<Utc>) -> Result<Role, Error> {
        let account_id = self.state.account_id();
        let holder = self.holder.as_ref().ok_or(Error::Facts {
            rule: "a current account retains its institutional holder",
        })?;
        let holder = history::state::<holder::State>(
            holder,
            roles.app.keys(),
            roles.anchors(),
            self.read,
            now,
        )?;
        if !holder.active()
            || holder.account_id() != account_id
            || holder.person_id() != self.person_id()?
        {
            return Err(Error::Facts {
                rule: "the institutional holder belongs to its live account",
            });
        }
        let row = self.role.as_ref().ok_or(Error::Facts {
            rule: "a current institutional holder retains its role",
        })?;
        let state = roles.state(row, self.read, now)?;
        if state.role().id() != holder.role_id()
            || state.scope() != Scope::Institution
            || !state.active()
        {
            return Err(Error::Facts {
                rule: "the current institutional holder retains a live institutional role",
            });
        }
        Ok(state.role().clone())
    }
}

pub(in crate::access) async fn find_by_actor(
    client: &Client,
    roles: &Reader,
    current: &current::Service,
    actor: &Actor,
) -> Result<Snapshot, Error> {
    let read = roles.read()?;
    let identity = current.fetch_state(client, actor, read).await?;
    find_by_identity(client, roles, identity).await
}

pub(in crate::access) async fn find_by_identity(
    client: &Client,
    roles: &Reader,
    identity: current::database::State,
) -> Result<Snapshot, Error> {
    let read = roles.read()?;
    let holder = holder::fetch(client, identity.account_id()).await?;
    let role = match &holder {
        Some(holder) => {
            client
                .query_opt(
                    &format!("{} WHERE state.id = $1", state::State::SELECT),
                    &[&holder.try_get::<_, Id>("role_id")?],
                )
                .await?
        }
        None => None,
    };
    Ok(Snapshot {
        state: identity,
        holder,
        role,
        read,
    })
}

pub(in crate::access) async fn find_by_id(
    client: &Client,
    roles: &Reader,
    id: Id,
    scope: Scope,
    now: DateTime<Utc>,
) -> Result<Option<Role>, Error> {
    roles.find(client, id, scope, now).await
}

pub(in crate::access) async fn lock_shared(
    client: &Client,
    id: Id,
    scope: Scope,
) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM access.roles WHERE id = $1 AND scope = $2 FOR SHARE",
            &[&id, &scope.as_ref()],
        )
        .await?;
    Ok(())
}

pub(super) async fn lock(client: &Client, id: Id, scope: Scope) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM access.roles WHERE id = $1 AND scope = $2 FOR UPDATE",
            &[&id, &scope.as_ref()],
        )
        .await?;
    Ok(())
}

pub(super) async fn lock_holders(
    client: &Client,
    id: Id,
    account_id: crate::identity::account::Id,
) -> Result<(), Error> {
    holder::lock_role_holders(client, id, account_id).await?;
    Ok(())
}

pub(super) async fn holder_levels(
    client: &Client,
    roles: &Reader,
    id: Id,
    now: DateTime<Utc>,
) -> Result<Vec<super::Level>, Error> {
    let mut levels = Vec::new();
    let grants =
        crate::access::grant::state::holders(client, roles, id, roles.read()?, now).await?;
    for grant in grants {
        let person_id = grant.person_id();
        let account = account::state::located(
            client,
            roles.keys(),
            roles.anchors(),
            roles.read()?,
            account::state::Location::Person(person_id),
            now,
        )
        .await?
        .filter(|account| account.current().is_some())
        .ok_or(Error::Facts {
            rule: "every retained Workforce holder has a current account",
        })?;
        let holder = roles
            .holder(client, account.id(), now)
            .await?
            .ok_or(Error::Facts {
                rule: "every retained Workforce holder has an institutional holder",
            })?;
        if !holder.active() || holder.person_id() != person_id {
            return Err(Error::Facts {
                rule: "a Workforce holder retains the same active institution identity",
            });
        }
        let role = roles
            .find(client, holder.role_id(), Scope::Institution, now)
            .await?
            .ok_or(Error::Facts {
                rule: "every retained Workforce holder has a live institutional role",
            })?;
        levels.push(role.level());
    }
    Ok(levels)
}

pub(super) async fn in_use(
    client: &Client,
    roles: &Reader,
    id: Id,
    now: DateTime<Utc>,
) -> Result<bool, super::CommandError> {
    if roles
        .holders(client, now)
        .await?
        .iter()
        .any(|holder| holder.active() && holder.role_id() == id)
    {
        return Ok(true);
    }
    if !crate::access::grant::state::holders(client, roles, id, roles.read()?, now)
        .await?
        .is_empty()
    {
        return Ok(true);
    }
    Ok(crate::access::grant::invitation::in_use(client, roles, id, now).await?)
}

pub(in crate::access) async fn fetch_list(
    client: &Client,
    roles: &Reader,
    pagination: Pagination,
    scope: Scope,
    now: DateTime<Utc>,
) -> Result<List, Error> {
    let values = roles
        .all(client, now)
        .await?
        .into_iter()
        .filter(|state| state.active() && state.scope() == scope)
        .map(|state| state.role().clone())
        .collect();
    page(values, pagination)
}

pub(in crate::access) async fn fetch_assignable(
    client: &Client,
    roles: &Reader,
    pagination: Pagination,
    scope: Scope,
    actor: &Role,
    now: DateTime<Utc>,
) -> Result<List, Error> {
    let values = roles
        .all(client, now)
        .await?
        .into_iter()
        .filter(|state| {
            state.active()
                && state.scope() == scope
                && state.role().level() < actor.level()
                && state
                    .role()
                    .permissions()
                    .iter()
                    .all(|code| actor.permissions().contains(code))
        })
        .map(|state| state.role().clone())
        .collect();
    page(values, pagination)
}

fn page(mut roles: Vec<Role>, pagination: Pagination) -> Result<List, Error> {
    roles.sort_by(|left, right| {
        right
            .level
            .cmp(&left.level)
            .then_with(|| left.name.0.cmp(&right.name.0))
            .then_with(|| left.id.as_bytes().cmp(right.id.as_bytes()))
    });
    let count = i64::try_from(roles.len()).map_err(|_| Error::Facts {
        rule: "role collection size is representable",
    })?;
    let offset = usize::try_from(pagination.offset()).unwrap_or(usize::MAX);
    let size = usize::try_from(pagination.size()).map_err(|_| Error::Facts {
        rule: "validated page size is representable",
    })?;
    Ok(List {
        count,
        roles: roles.into_iter().skip(offset).take(size).collect(),
    })
}

/// Assignments retain the exact definition selected within their capability scope.
pub(crate) async fn definitions(
    client: &impl GenericClient,
    keys: history::Keys<'_>,
    selections: &[(Id, change::Id)],
    scope: Scope,
    now: DateTime<Utc>,
) -> Result<Vec<Role>, Error> {
    let (ids, changes): (Vec<_>, Vec<_>) = selections.iter().copied().unzip();
    let statement = format!(
        "SELECT {} FROM unnest($1::uuid[], $2::uuid[]) WITH ORDINALITY AS selected (role_id, change_id, position) LEFT JOIN LATERAL ({} WHERE record.id=selected.change_id AND record.role_id=selected.role_id AND record.scope=$3) record ON true ORDER BY selected.position",
        change::COLUMNS,
        change::Record::SELECT,
    );
    let rows = client
        .query(&statement, &[&ids, &changes, &scope.as_ref()])
        .await?;
    rows.iter()
        .map(|row| history::record::<change::Record>(row, keys, now)?.definition())
        .collect()
}
