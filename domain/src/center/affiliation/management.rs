//! Candidate disclosure and qualified management projections.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};
use tokio_postgres::Client;
use utoipa::ToSchema;

use crate::{
    access::{appointment, role},
    center::affiliation::{
        self, Code, Error, Rejection, Request, Service, command, database, query,
    },
    history::View,
    identity::{account, current::Actor, mobile::Mobile, person, person::revision},
    pagination::Pagination,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Account(account::Id),
    Person,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = affiliation::management::Input)]
pub struct Input {
    pub(super) target: Confirmation,
    pub(super) affiliation: Code,
}

#[derive(Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[schema(as = affiliation::management::Confirmation)]
pub(super) enum Confirmation {
    #[serde(rename_all = "camelCase")]
    Account {
        account_id: account::Id,
        person_id: person::Id,
        revision_id: revision::Id,
        #[serde(default, deserialize_with = "mobile")]
        #[schema(value_type = String, required = false, nullable = false, pattern = "^1[0-9]{10}$")]
        mobile: Option<Mobile>,
    },
    #[serde(rename_all = "camelCase")]
    Person {
        person_id: person::Id,
        revision_id: revision::Id,
    },
}

fn mobile<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Mobile>, D::Error> {
    Mobile::deserialize(deserializer).map(Some)
}

impl Confirmation {
    pub(super) fn person_id(&self) -> person::Id {
        match self {
            Self::Account { person_id, .. } | Self::Person { person_id, .. } => *person_id,
        }
    }

    pub(super) fn revision_id(&self) -> revision::Id {
        match self {
            Self::Account { revision_id, .. } | Self::Person { revision_id, .. } => *revision_id,
        }
    }
}

pub struct Candidate {
    target: Target,
    identity: revision::Identity,
}

impl Candidate {
    pub fn target(&self) -> Target {
        self.target
    }
    pub fn identity(&self) -> &revision::Identity {
        &self.identity
    }
}

pub struct List {
    pub(super) count: i64,
    pub(super) affiliations: Vec<Affiliation>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn affiliations(&self) -> &[Affiliation] {
        &self.affiliations
    }
}

pub struct Affiliation {
    value: Code,
    created_at: DateTime<Utc>,
    operator: revision::Identity,
}

impl Affiliation {
    pub fn value(&self) -> Code {
        self.value
    }
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub fn operator(&self) -> &revision::Identity {
        &self.operator
    }
}

impl Service {
    pub async fn correct(
        &self,
        actor: Actor,
        person_id: person::Id,
        input: Input,
    ) -> Result<(), Error> {
        if input.target.person_id() != person_id {
            return Err(Rejection::TargetChanged.into());
        }
        command::run(self, actor, Request::Correct { input }).await
    }

    async fn authorize_candidate(
        &self,
        client: &Client,
        actor: &Actor,
        view: View,
    ) -> Result<(role::Role, person::Id, DateTime<Utc>), Error> {
        let (person, _, now) = self.current.at(view).fetch_person(client, actor).await?;
        let current =
            appointment::find_by_account(client, &self.roles.at(view), actor.account_id(), now)
                .await?
                .ok_or(Error::corrupt(
                    "the actor retains its institutional assignment",
                ))?;
        if !current
            .role()
            .permissions()
            .contains(&role::Code::InstitutionAffiliationsRead)
            && !current
                .role()
                .permissions()
                .contains(&role::Code::InstitutionAffiliationsCorrect)
        {
            return Err(Error::Denied);
        }
        Ok((current.into_role(), person, now))
    }

    async fn authorize_account(
        client: &Client,
        roles: &role::Reader,
        role: &role::Role,
        account: account::Id,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        if !role
            .permissions()
            .contains(&role::Code::InstitutionAffiliationsRead)
        {
            let target = appointment::find_by_account(client, roles, account, now)
                .await?
                .ok_or(Error::corrupt(
                    "the located account retains its institutional assignment",
                ))?;
            if target.role().level() >= role.level() {
                return Err(Rejection::Absent.into());
            }
        }
        Ok(())
    }

    pub async fn candidate_by_mobile(
        &self,
        actor: &Actor,
        mobile: Mobile,
    ) -> Result<Candidate, Error> {
        let service = self.clone();
        let actor = actor.clone();
        query::snapshot(self, move |client, view| {
            let service = service.clone();
            let actor = actor.clone();
            let mobile = mobile.clone();
            Box::pin(async move {
                service
                    .candidate(client, &actor, Locator::Mobile(&mobile), view)
                    .await
            })
        })
        .await
    }

    pub async fn candidate_by_person(
        &self,
        actor: &Actor,
        person_id: person::Id,
    ) -> Result<Candidate, Error> {
        let service = self.clone();
        let actor = actor.clone();
        query::snapshot(self, move |client, view| {
            let service = service.clone();
            let actor = actor.clone();
            Box::pin(async move {
                service
                    .candidate(client, &actor, Locator::Person(person_id), view)
                    .await
            })
        })
        .await
    }

    pub async fn read_person(
        &self,
        actor: &Actor,
        person_id: person::Id,
    ) -> Result<affiliation::Affiliation, Error> {
        let service = self.clone();
        let actor = actor.clone();
        query::snapshot(self, move |client, view| {
            let service = service.clone();
            let actor = actor.clone();
            Box::pin(async move {
                query::current(&service, client, &actor, Some(person_id), view).await
            })
        })
        .await
    }

    pub async fn history_by_person(
        &self,
        actor: &Actor,
        person_id: person::Id,
        pagination: Pagination,
    ) -> Result<List, Error> {
        let service = self.clone();
        let actor = actor.clone();
        query::snapshot(self, move |client, view| {
            let service = service.clone();
            let actor = actor.clone();
            Box::pin(async move {
                let authority =
                    query::authorize(&service, client, &actor, Some(person_id), view).await?;
                let (count, records) =
                    database::fetch_history(client, authority.person, pagination, authority.now)
                        .await?;
                let ids = records
                    .iter()
                    .map(database::Record::operator_id)
                    .collect::<Vec<_>>();
                let identities = service.revisions.fetch_identities(client, &ids).await?;
                let mut affiliations = Vec::with_capacity(records.len());
                for record in records {
                    let operator = identities
                        .get(&record.operator_id())
                        .ok_or(revision::ReadError::Missing)?;
                    affiliations.push(Affiliation {
                        value: record.code(service.app.keys().data)?,
                        created_at: record.created_at(),
                        operator: operator.clone(),
                    });
                }
                Ok(List {
                    count,
                    affiliations,
                })
            })
        })
        .await
    }

    async fn candidate(
        &self,
        client: &Client,
        actor: &Actor,
        locator: Locator<'_>,
        view: View,
    ) -> Result<Candidate, Error> {
        let (role, operator, now) = self.authorize_candidate(client, actor, view).await?;
        match locator {
            Locator::Mobile(mobile) => {
                let account = self
                    .accounts
                    .at(view)
                    .find_by_mobile(client, mobile, now)
                    .await?
                    .ok_or(Rejection::Absent)?;
                if account == actor.account_id() {
                    return Err(Rejection::Absent.into());
                }
                Self::authorize_account(client, &self.roles.at(view), &role, account, now).await?;
                let subject = self
                    .accounts
                    .at(view)
                    .find_by_id(client, account, mobile, now)
                    .await?
                    .ok_or(Rejection::Absent)?;
                Ok(Candidate {
                    target: Target::Account(account),
                    identity: subject.into(),
                })
            }
            Locator::Person(person) => {
                if person == operator {
                    return Err(Rejection::Absent.into());
                }
                let target = match self
                    .accounts
                    .at(view)
                    .find_by_person(client, person, now)
                    .await?
                {
                    Some(account) => {
                        Self::authorize_account(client, &self.roles.at(view), &role, account, now)
                            .await?;
                        Target::Account(account)
                    }
                    None => Target::Person,
                };
                let identity = self
                    .persons
                    .with_read(self.roles.at(view).read()?)
                    .find_identity(client, person, now)
                    .await?
                    .ok_or(Rejection::Absent)?;
                Ok(Candidate { target, identity })
            }
        }
    }
}

enum Locator<'a> {
    Mobile(&'a Mobile),
    Person(person::Id),
}
