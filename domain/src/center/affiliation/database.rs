//! Fixed queries for permanent encrypted values. Current is selected by effective time.

use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::time::timeout;
use tokio_postgres::{Client, Row};

use crate::{
    center::affiliation::{Affiliation, Code, Id, envelope},
    database::sealed::Sealed,
    identity::{account, current::Actor, person, person::revision},
    kernel::{Error, Fault, FaultKind, Stage, keys::Keys},
    pagination::Pagination,
};

pub(crate) const TABLE: &str = "center.affiliations";

/// Keep the existing startup safety boundary while reading only the retained fact shape.
pub(super) async fn verify(
    client: &Client,
    keys: &Keys,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<(), crate::center::affiliation::Error> {
    let mut cursor: Option<Id> = None;
    loop {
        let records = match timeout(Duration::from_secs(10), client.query(
            "SELECT id, person_id, value, operator_id, created_at FROM center.affiliations WHERE ($1::uuid IS NULL OR id>$1) ORDER BY id LIMIT 128",
            &[&cursor],
        )).await {
            Ok(result) => result.map_err(Error::query)?,
            Err(error) => return Error::elapsed(error),
        }.into_iter().map(Record::try_from).collect::<Result<Vec<_>, _>>().map_err(decode)?;
        if records.is_empty() {
            return Ok(());
        }
        let mut operators = records.iter().map(Record::operator_id).collect::<Vec<_>>();
        operators.sort_unstable_by_key(|id| *id.as_bytes());
        operators.dedup();
        revisions.fetch_identities(client, &operators).await?;
        for record in records {
            if record.created_at > now {
                return Err(Error::Corrupt(Fault::new(
                    Stage::Read,
                    FaultKind::Integrity("an affiliation has a non-future effective time"),
                )));
            }
            record.code::<crate::center::affiliation::Rejection>(keys)?;
            cursor = Some(record.id);
        }
    }
}

pub(super) struct Record {
    id: Id,
    person_id: person::Id,
    value: Sealed,
    operator_id: revision::Id,
    created_at: DateTime<Utc>,
}

impl TryFrom<Row> for Record {
    type Error = tokio_postgres::Error;

    fn try_from(row: Row) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            person_id: row.try_get("person_id")?,
            value: row.try_get("value")?,
            operator_id: row.try_get("operator_id")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

impl Record {
    pub(super) fn id(&self) -> Id {
        self.id
    }

    pub(super) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub(super) fn operator_id(&self) -> revision::Id {
        self.operator_id
    }

    pub(super) fn code<R>(&self, keys: &Keys) -> Result<Code, Error<R>> {
        envelope::open(keys, self.person_id, self.id, &self.value)
    }

    pub(super) fn affiliation<R>(&self, keys: &Keys) -> Result<Affiliation, Error<R>> {
        Ok(Affiliation {
            value: self.code(keys)?,
            created_at: self.created_at,
        })
    }
}

pub(super) async fn find_latest<R>(
    client: &Client,
    person_id: person::Id,
    now: DateTime<Utc>,
) -> Result<Option<Record>, Error<R>> {
    let record = client
        .query_opt(
            "SELECT id, person_id, value, operator_id, created_at FROM center.affiliations WHERE person_id=$1 ORDER BY created_at DESC LIMIT 1",
            &[&person_id],
        )
        .await
        .map_err(Error::query)?
        .map(Record::try_from)
        .transpose()
        .map_err(decode)?;
    if record
        .as_ref()
        .is_some_and(|record| record.created_at > now)
    {
        return Err(Error::Corrupt(Fault::new(
            Stage::Read,
            FaultKind::Integrity("an affiliation has a non-future effective time"),
        )));
    }
    Ok(record)
}

pub(super) async fn fetch_history<R>(
    client: &Client,
    person_id: person::Id,
    pagination: Pagination,
    now: DateTime<Utc>,
) -> Result<(i64, Vec<Record>), Error<R>> {
    let row = client
        .query_one(
            "SELECT count(*), max(created_at) FROM center.affiliations WHERE person_id=$1",
            &[&person_id],
        )
        .await
        .map_err(Error::query)?;
    let count = row.try_get(0).map_err(decode)?;
    let latest: Option<DateTime<Utc>> = row.try_get(1).map_err(decode)?;
    if latest.is_some_and(|time| time > now) {
        return Err(Error::Corrupt(Fault::new(
            Stage::Read,
            FaultKind::Integrity("affiliation history has non-future effective times"),
        )));
    }
    let records = client
        .query(
            "SELECT id, person_id, value, operator_id, created_at FROM center.affiliations WHERE person_id=$1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
            &[&person_id, &pagination.size(), &pagination.offset()],
        )
        .await
        .map_err(Error::query)?
        .into_iter()
        .map(Record::try_from)
        .collect::<Result<_, _>>()
        .map_err(decode)?;
    Ok((count, records))
}

pub(super) async fn insert<R>(
    client: &Client,
    keys: &Keys,
    person_id: person::Id,
    operator_id: revision::Id,
    id: Id,
    code: Code,
    created_at: DateTime<Utc>,
) -> Result<(), crate::kernel::Error<R>> {
    let value = envelope::seal(keys, person_id, id, code)?;
    client
        .execute(
            "INSERT INTO center.affiliations (id, person_id, value, operator_id, created_at) VALUES ($1,$2,$3,$4,$5)",
            &[&id, &person_id, &value, &operator_id, &created_at],
        )
        .await
        .map_err(crate::kernel::Error::query)?;
    Ok(())
}

pub(super) async fn audit<R>(
    client: &Client,
    actor: person::Id,
    action: &str,
    id: Id,
) -> Result<(), crate::kernel::Error<R>> {
    client
        .execute(
            "INSERT INTO audit.events (actor_id, action, target_type, target_id, details) VALUES ($1,$2,'center.affiliation',$3,'{}'::jsonb)",
            &[&actor, &action, &id],
        )
        .await
        .map_err(crate::kernel::Error::query)?;
    Ok(())
}

pub(super) async fn lock<R>(client: &Client, actor: &Actor) -> Result<(), Error<R>> {
    actor.lock_account(client).await?;
    account::state::lock_person(client, actor.account_id()).await?;
    Ok(())
}

pub(super) async fn person_exists<R>(
    client: &Client,
    person_id: person::Id,
) -> Result<bool, Error<R>> {
    Ok(person::state::exists(client, person_id).await?)
}

pub(super) async fn lock_accounts<R>(
    client: &Client,
    actor: account::Id,
    target: account::Id,
) -> Result<(), Error<R>> {
    let mut ids = [actor, target];
    ids.sort_unstable_by_key(|id| *id.as_bytes());
    for id in ids {
        client
            .query_one("SELECT identity.lock_account($1)", &[&id])
            .await
            .map_err(Error::query)?;
    }
    Ok(())
}

fn decode<R>(error: tokio_postgres::Error) -> Error<R> {
    Error::Corrupt(Fault::new(Stage::Read, FaultKind::Decode(error.into())))
}
