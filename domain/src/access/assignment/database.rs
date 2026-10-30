//! Fixed role definitions and identities projected from authenticated Grant records.

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    access::{
        assignment::{Assignment, Error, Filter, List, ReadError},
        capability::Scope,
        grant::record::{Event, Record},
        role,
    },
    history::{self, Kind as _, Record as _, Verified},
    identity::{operation, person::revision},
};

pub(super) async fn deliver(
    client: &Client,
    roles: &role::Reader,
    record: &Verified<Record>,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<Assignment, Error> {
    let Event::Assignment {
        change_id,
        operator_id,
        revision_id,
        ..
    } = record.event()
    else {
        return Err(Error::Facts {
            rule: "an assignment result names an assignment record",
        });
    };
    let role = role::database::definitions(
        client,
        roles.keys(),
        &[(record.role_id(), *change_id)],
        Scope::Workforce,
        now,
    )
    .await?
    .pop()
    .ok_or(Error::Facts {
        rule: "an assignment retains its selected role definition",
    })?;
    let subject = revisions
        .fetch_identity(client, *revision_id)
        .await
        .map_err(ReadError::from)?;
    if subject.person_id() != record.person_id() {
        return Err(Error::Facts {
            rule: "an assignment retains its fixed subject identity",
        });
    }
    Ok(Assignment {
        id: record.assignment_id(),
        grant_id: record.grant_id(),
        subject,
        operator: revisions
            .fetch_identity(client, *operator_id)
            .await
            .map_err(ReadError::from)?,
        role,
        sequence: record.sequence(),
        assigned_at: record.happened_at(),
        operation_id: record.operation_id(),
    })
}

pub(super) async fn fetch_list(
    client: &Client,
    roles: &role::Reader,
    filter: &Filter,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<List, Error> {
    let count = client.query_one("SELECT count(*) FROM access.assignments WHERE ($1::uuid IS NULL OR person_id=$1) AND ($2::uuid IS NULL OR operation_id=$2)", &[&filter.person_id, &filter.operation_id]).await?.try_get(0).map_err(Error::decode)?;
    let rows = client.query(&format!("{} WHERE record.kind='assignment' AND ($1::uuid IS NULL OR record.person_id=$1) AND ($2::uuid IS NULL OR record.operation_id=$2) ORDER BY record.happened_at DESC,record.id DESC LIMIT $3 OFFSET $4", Record::SELECT), &[&filter.person_id, &filter.operation_id, &filter.pagination.size(), &filter.pagination.offset()]).await?;
    let records = rows
        .iter()
        .map(|row| history::record::<Record>(row, roles.keys(), now))
        .collect::<Result<Vec<_>, _>>()?;
    let mut selections = Vec::with_capacity(records.len());
    let mut ids = Vec::with_capacity(records.len() * 2);
    for record in &records {
        let Event::Assignment {
            change_id,
            operator_id,
            revision_id,
            ..
        } = record.event()
        else {
            return Err(Error::Facts {
                rule: "an assignment page contains assignment records",
            });
        };
        selections.push((record.role_id(), *change_id));
        ids.extend([*revision_id, *operator_id]);
    }
    let definitions =
        role::database::definitions(client, roles.keys(), &selections, Scope::Workforce, now)
            .await?;
    let identities = revisions
        .fetch_identities(client, &ids)
        .await
        .map_err(ReadError::from)?;
    let mut assignments = Vec::with_capacity(records.len());
    for (record, role) in records.into_iter().zip(definitions) {
        let Event::Assignment {
            operator_id,
            revision_id,
            ..
        } = record.event()
        else {
            return Err(Error::Facts {
                rule: "an assignment page retains its record kind",
            });
        };
        let subject = identities
            .get(revision_id)
            .ok_or(ReadError::Revision(revision::ReadError::Missing))?
            .clone();
        if subject.person_id() != record.person_id() {
            return Err(Error::Facts {
                rule: "an assignment retains its fixed subject identity",
            });
        }
        assignments.push(Assignment {
            id: record.assignment_id(),
            grant_id: record.grant_id(),
            subject,
            operator: identities
                .get(operator_id)
                .ok_or(ReadError::Revision(revision::ReadError::Missing))?
                .clone(),
            role,
            sequence: record.sequence(),
            assigned_at: record.happened_at(),
            operation_id: record.operation_id(),
        });
    }
    Ok(List { count, assignments })
}

pub(super) async fn original(
    client: &Client,
    roles: &role::Reader,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<Verified<Record>>, Error> {
    let row = client.query_opt(&format!("{} WHERE record.kind='assignment' AND record.sequence>1 AND record.operation_id=$1", Record::SELECT), &[&operation_id]).await?;
    row.as_ref()
        .map(|row| history::record::<Record>(row, roles.keys(), now).map_err(Error::from))
        .transpose()
}
