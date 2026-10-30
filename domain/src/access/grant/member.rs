//! Confirm current membership and qualify domain management in the owning transaction.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::timeout_at;
use tokio_postgres::Client;
use utoipa::ToSchema;

use crate::{
    access::{
        capability::Scope,
        grant::{Error, Id, Service, state},
        role,
    },
    history,
    identity::{
        account,
        current::{self, Actor},
        mobile::Mobile,
        person::{self, revision},
    },
};

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = grant::member::Target)]
pub struct Target {
    pub(super) account_id: account::Id,
    pub(super) person_id: person::Id,
    pub(super) revision_id: revision::Id,
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    #[serde(serialize_with = "serialize_mobile")]
    pub(super) mobile: Mobile,
}

fn serialize_mobile<S: serde::Serializer>(
    mobile: &Mobile,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(mobile.as_ref())
}

impl Target {
    pub(crate) fn account_id(&self) -> &account::Id {
        &self.account_id
    }
    pub(crate) fn person_id(&self) -> &person::Id {
        &self.person_id
    }
    pub(crate) fn revision_id(&self) -> &revision::Id {
        &self.revision_id
    }
    pub(crate) fn mobile(&self) -> &Mobile {
        &self.mobile
    }
}

pub struct Candidate {
    identity: account::Subject,
    grant_id: Id,
}

impl Candidate {
    pub fn identity(&self) -> &account::Subject {
        &self.identity
    }
    pub fn grant_id(&self) -> Id {
        self.grant_id
    }
}

pub(crate) struct Member {
    id: Id,
    person_id: person::Id,
    role: role::Role,
    assignment_id: crate::access::assignment::Id,
    suspended: bool,
}

impl Member {
    pub(crate) fn id(&self) -> Id {
        self.id
    }
    pub(in crate::access) fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub(crate) fn suspended(&self) -> bool {
        self.suspended
    }
    pub(in crate::access) fn into_role(self) -> role::Role {
        self.role
    }

    pub(in crate::access) fn role(&self) -> &role::Role {
        &self.role
    }

    pub(in crate::access) fn assignment_id(&self) -> crate::access::assignment::Id {
        self.assignment_id
    }

    fn require_active(&self) -> Result<(), Error> {
        if self.suspended {
            return Err(Error::Denied);
        }
        Ok(())
    }

    pub(in crate::access) fn authorize(self, permission: role::Code) -> Result<role::Role, Error> {
        self.require_active()?;
        if !self.role.permissions().contains(&permission) {
            return Err(Error::Denied);
        }
        Ok(self.role)
    }
}

pub(crate) async fn find_by_person(
    client: &Client,
    roles: &role::Reader,
    person_id: person::Id,
    now: DateTime<Utc>,
) -> Result<Option<Member>, Error> {
    let Some(grant) = state::current(client, roles, person_id, roles.read()?, now).await? else {
        return Ok(None);
    };
    let accounts = account::state::for_persons(
        client,
        roles.keys(),
        roles.anchors(),
        roles.read()?,
        &[person_id],
        now,
    )
    .await?;
    if !accounts
        .first()
        .and_then(Option::as_ref)
        .is_some_and(|account| account.current().is_some())
    {
        return Err(Error::Facts {
            rule: "an unfinished grant retains its authenticated account",
        });
    }
    let role = role::database::find_by_id(client, roles, grant.role_id(), Scope::Workforce, now)
        .await?
        .ok_or(Error::Facts {
            rule: "current membership retains its Workforce role definition",
        })?;
    Ok(Some(Member {
        id: grant.id(),
        person_id,
        role,
        assignment_id: grant.assignment_id(),
        suspended: grant.suspension_id().is_some(),
    }))
}

/// Holds the account's current Grant against concurrent suspension, resumption or termination.
pub(crate) async fn lock_current(client: &Client, account_id: account::Id) -> Result<(), Error> {
    Ok(state::lock_operator(client, account_id).await?)
}

/// The person's current unsuspended Workforce grant, for capabilities that require participation.
pub(crate) async fn find_active(
    client: &Client,
    roles: &role::Reader,
    person_id: person::Id,
    now: DateTime<Utc>,
) -> Result<Option<Id>, Error> {
    Ok(find_by_person(client, roles, person_id, now)
        .await?
        .filter(|member| !member.suspended)
        .map(|member| member.id))
}

pub(in crate::access) async fn find_by_account(
    client: &Client,
    roles: &role::Reader,
    account_id: account::Id,
    now: DateTime<Utc>,
) -> Result<Option<Member>, Error> {
    let Some(account) = history::find::<account::state::State>(
        client,
        roles.keys(),
        roles.anchors(),
        roles.read()?,
        account_id,
        now,
    )
    .await?
    .filter(|account| account.current().is_some()) else {
        return Ok(None);
    };
    find_by_person(client, roles, account.person_id(), now).await
}

pub(crate) async fn authorize_participation(
    client: &Client,
    roles: &role::Reader,
    current: &current::Service,
    actor: &Actor,
) -> Result<(), Error> {
    let state = current.fetch_state(client, actor, roles.read()?).await?;
    let (person_id, now) = (state.person_id(), state.observed_at());
    find_by_person(client, roles, person_id, now)
        .await?
        .ok_or(Error::Denied)?
        .require_active()
}

pub(crate) async fn authorize(
    client: &Client,
    roles: &role::Reader,
    current: &current::Service,
    actor: &Actor,
    permission: role::Code,
) -> Result<(role::Role, person::Id, revision::Id, DateTime<Utc>), Error> {
    let state = current.fetch_state(client, actor, roles.read()?).await?;
    authorize_subject(client, roles, state, permission).await
}

pub(crate) async fn authorize_subject(
    client: &Client,
    roles: &role::Reader,
    state: current::database::State,
    permission: role::Code,
) -> Result<(role::Role, person::Id, revision::Id, DateTime<Utc>), Error> {
    let (person_id, revision_id, now) =
        (state.person_id(), state.revision_id(), state.observed_at());
    let member = find_by_person(client, roles, person_id, now)
        .await?
        .ok_or(Error::Denied)?;
    Ok((member.authorize(permission)?, person_id, revision_id, now))
}

impl Service {
    pub(super) async fn authorize_member(
        &self,
        client: &Client,
        roles: &role::Reader,
        actor: &Actor,
        permission: role::Code,
    ) -> Result<(role::Role, person::Id, revision::Id, DateTime<Utc>), Error> {
        authorize(client, roles, &self.current, actor, permission).await
    }

    pub(super) async fn fetch_managed_member(
        client: &Client,
        roles: &role::Reader,
        manager: &role::Role,
        operator_id: person::Id,
        account_id: account::Id,
        now: DateTime<Utc>,
    ) -> Result<Member, Error> {
        let member = find_by_account(client, roles, account_id, now)
            .await?
            .ok_or(Error::Absent)?;
        if operator_id == member.person_id || member.role.level() >= manager.level() {
            return Err(Error::Absent);
        }
        Ok(member)
    }

    pub(super) async fn member_candidate(
        &self,
        actor: &Actor,
        mobile: Mobile,
        permission: role::Code,
    ) -> Result<Candidate, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let mobile = &mobile;
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    async move {
                        let (manager, person_id, _, now) = self
                            .authorize_member(client, &roles, actor, permission)
                            .await?;
                        let account_id = self
                            .accounts
                            .at(view)
                            .find_by_mobile(client, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        let member = Self::fetch_managed_member(
                            client, &roles, &manager, person_id, account_id, now,
                        )
                        .await?;
                        let identity = self
                            .accounts
                            .at(view)
                            .find_by_id(client, account_id, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        Ok(Candidate {
                            identity,
                            grant_id: member.id,
                        })
                    }
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }
}
