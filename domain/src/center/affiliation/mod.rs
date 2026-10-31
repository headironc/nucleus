//! Person-owned encrypted affiliation history with qualified current-state operations.

pub mod admin;
mod code;
mod command;
mod database;
mod envelope;
pub mod management;
mod query;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;

pub(crate) use crate::center::affiliation::database::TABLE as AFFILIATIONS;
pub use crate::center::affiliation::{
    code::Code,
    management::{Candidate, Target},
};
use crate::{
    access::role,
    identity::{
        self, account,
        current::{self, Actor},
        person,
    },
    kernel::{self, App},
    pagination::Pagination,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql)]
#[serde(transparent)]
#[postgres(transparent)]
struct Id(Uuid);

impl std::fmt::Display for Id {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Id {
    fn allocate() -> Self {
        Self(Uuid::new_v4())
    }

    fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = affiliation::Input)]
pub struct Input {
    affiliation: Code,
}

enum Request {
    Fill(Code),
    Correct { input: management::Input },
}

impl Request {
    fn code(&self) -> Code {
        match self {
            Self::Fill(code) => *code,
            Self::Correct { input } => input.affiliation,
        }
    }

    fn action(&self) -> &'static str {
        match self {
            Self::Fill(_) => "fill",
            Self::Correct { .. } => "correct",
        }
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = affiliation::Affiliation)]
pub struct Affiliation {
    value: Code,
    created_at: DateTime<Utc>,
}

pub struct List {
    count: i64,
    affiliations: Vec<Affiliation>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }

    pub fn affiliations(&self) -> &[Affiliation] {
        &self.affiliations
    }
}

#[derive(Clone)]
pub struct Service {
    app: App,
    current: current::Service,
    roles: role::Reader,
    accounts: account::Reader,
    persons: person::Reader,
    revisions: person::revision::Reader,
}

/// Startup retains value-envelope and original-identity checks without obsolete state seals.
pub(crate) async fn verify(
    client: &tokio_postgres::Client,
    app: &App,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    database::verify(
        client,
        app.keys().data,
        &person::revision::Reader::new(app.clone()),
        now,
    )
    .await
}

/// Disposable fixtures use the same envelope and audit writer as real first fills.
#[cfg(any(test, feature = "test-support"))]
pub async fn seed(
    client: &tokio_postgres::Client,
    keys: &crate::keys::Keys,
    person_id: person::Id,
    operator_id: person::revision::Id,
    code: Code,
) -> Result<(), Error> {
    let created_at: DateTime<Utc> = client
        .query_one("SELECT clock_timestamp()", &[])
        .await
        .map_err(Error::query)?
        .try_get(0)
        .map_err(Error::decode)?;
    let id = Id::allocate();
    let operator = person::revision::fetch(
        client,
        crate::history::Keys {
            data: &keys.encryption,
            seals: &keys.seal,
        },
        operator_id,
    )
    .await
    .map_err(person::revision::ReadError::from)?;
    database::insert(
        client,
        &keys.encryption,
        person_id,
        operator_id,
        id,
        code,
        created_at,
    )
    .await?;
    database::audit(client, operator.person_id(), "fill", id).await
}

impl Service {
    pub(crate) fn prepare(
        app: App,
        current: current::Service,
        identity: Arc<identity::keys::Keys>,
    ) -> Self {
        let accounts = account::Reader::new(app.clone(), Arc::clone(&identity));
        Self {
            roles: role::Reader::new(app.clone()),
            app: app.clone(),
            current,
            accounts: accounts.clone(),
            persons: person::Reader::new(accounts, Arc::clone(&identity)),
            revisions: person::revision::Reader::new(app),
        }
    }

    pub async fn values(&self, actor: &Actor) -> Result<&'static [Code], Error> {
        let service = self.clone();
        let actor = actor.clone();
        query::snapshot(self, move |client, view| {
            let service = service.clone();
            let actor = actor.clone();
            Box::pin(async move {
                service
                    .current
                    .at(view)
                    .fetch_person(client, &actor)
                    .await?;
                Ok(Code::all())
            })
        })
        .await
    }

    pub async fn read(&self, actor: &Actor) -> Result<Affiliation, Error> {
        let service = self.clone();
        let actor = actor.clone();
        query::snapshot(self, move |client, view| {
            let service = service.clone();
            let actor = actor.clone();
            Box::pin(async move { query::current(&service, client, &actor, None, view).await })
        })
        .await
    }

    pub async fn history(&self, actor: &Actor, pagination: Pagination) -> Result<List, Error> {
        let service = self.clone();
        let actor = actor.clone();
        query::snapshot(self, move |client, view| {
            let service = service.clone();
            let actor = actor.clone();
            Box::pin(async move {
                let authority = query::authorize(&service, client, &actor, None, view).await?;
                let (count, records) =
                    database::fetch_history(client, authority.person, pagination, authority.now)
                        .await?;
                let operators = records
                    .iter()
                    .map(database::Record::operator_id)
                    .collect::<Vec<_>>();
                service
                    .revisions
                    .fetch_identities(client, &operators)
                    .await?;
                let affiliations = records
                    .iter()
                    .map(|record| record.affiliation(service.app.keys().data))
                    .collect::<Result<_, _>>()?;
                Ok(List {
                    count,
                    affiliations,
                })
            })
        })
        .await
    }

    pub async fn create(&self, actor: Actor, input: Input) -> Result<(), Error> {
        command::run(self, actor, Request::Fill(input.affiliation)).await
    }
}

pub type Error = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Absent,
    Unset,
    Conflict,
    TargetChanged,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Absent | Self::Unset => kernel::Status::NotFound,
            Self::Conflict | Self::TargetChanged => kernel::Status::Conflict,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Absent => "target_unavailable",
            Self::Unset => "affiliation_unset",
            Self::Conflict => "state_conflict",
            Self::TargetChanged => "target_changed",
        }
    }
}

impl From<Rejection> for Error {
    fn from(rejection: Rejection) -> Self {
        Self::Rejected(rejection)
    }
}
