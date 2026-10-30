//! Startup binds authenticated records to their retained values and permanent completions.

use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    database::verification::Stage,
    history::{self, Record as _},
    identity::person,
    kernel::completion,
    workforce::number::{
        Error, command,
        record::{self, Event},
        revision,
        state::State,
    },
};

pub(super) async fn inspect(
    client: &Client,
    keys: history::Keys<'_>,
    identities: &person::revision::Reader,
    now: DateTime<Utc>,
) -> Result<Vec<history::Verified<State>>, Error> {
    let states = history::inspect::<State>(client, keys, now).await?;
    history::inspect_values::<revision::Value>(client, keys, now).await?;
    completion::verify_workforce_families(client).await?;
    let mut completions = HashMap::new();
    for (kind, template) in [
        ("allocation", command::ALLOCATIONS),
        ("correction", command::CORRECTIONS),
        ("recovery", command::RECOVERIES),
    ] {
        for completion in completion::fetch_all(client, keys, template).await? {
            completion.check_time(now)?;
            completions.insert((kind, *completion.operation_id().as_bytes()), completion);
        }
    }
    let mut person = None;
    let mut sequence = 0;
    loop {
        let batch = async {
            let records = record::batch(client, keys, person, sequence, now).await?;
            let operators = records
                .iter()
                .map(|record| record.operator_id())
                .collect::<Vec<_>>();
            let operators = identities.fetch_identities(client, &operators).await?;
            for record in &records {
                person = Some(record.person_id());
                sequence = record.sequence();
                let value =
                    history::find_value::<revision::Value>(client, keys, record.number_id(), now)
                        .await?
                        .ok_or_else(|| Error::corrupt("every number record retains its value"))?;
                if value.person_id() != record.person_id() {
                    return Err(Error::corrupt(
                        "number records and values belong to the same person",
                    ));
                }
                let (kind, action) = match record.event() {
                    Event::Allocation { source, .. } => ("allocation", record::source(*source)),
                    Event::Correction { before_id, .. } => {
                        let before =
                            history::find_value::<revision::Value>(client, keys, *before_id, now)
                                .await?
                                .ok_or_else(|| {
                                    Error::corrupt("a correction retains its previous value")
                                })?;
                        if before.person_id() != record.person_id() {
                            return Err(Error::corrupt(
                                "correction values belong to the same person",
                            ));
                        }
                        ("correction", "correct")
                    }
                    Event::Recovery {
                        origin_id,
                        occupier_id,
                        revocation_id,
                        ..
                    } => {
                        history::find_value::<revision::Value>(client, keys, *origin_id, now)
                            .await?
                            .ok_or_else(|| {
                                Error::corrupt("a recovery retains its original value")
                            })?;
                        for id in std::iter::once(*occupier_id).chain(*revocation_id) {
                            let revocation =
                                record::by_id(client, keys, "revocation", id, now).await?;
                            if !matches!(revocation.event(), Event::Revocation { recovery_id, .. } if *recovery_id == record.id())
                            {
                                return Err(Error::corrupt(
                                    "a recovery retains its fixed revocation group",
                                ));
                            }
                        }
                        ("recovery", "recover")
                    }
                    Event::Revocation { recovery_id, .. } => {
                        let recovery =
                            record::by_id(client, keys, "recovery", *recovery_id, now).await?;
                        if !matches!(recovery.event(), Event::Recovery { occupier_id, revocation_id, .. } if *occupier_id == record.id() || *revocation_id == Some(record.id()))
                        {
                            return Err(Error::corrupt(
                                "a revocation belongs to its fixed recovery group",
                            ));
                        }
                        continue;
                    }
                };
                let completion = completions
                    .remove(&(kind, *record.operation_id().as_bytes()))
                    .ok_or_else(|| {
                        Error::corrupt("every number command retains its permanent completion")
                    })?;
                let operator = operators
                    .get(&record.operator_id())
                    .ok_or(person::revision::ReadError::Missing)?;
                if completion.action() != action
                    || completion.target() != record.person_id().to_string()
                    || completion.person_id() != operator.person_id()
                    || completion.revision_id() != record.operator_id()
                    || completion.created_at() != record.time()
                {
                    return Err(Error::corrupt(
                        "a number completion agrees with its original actor, action, target and time",
                    ));
                }
            }
            Ok::<_, Error>(records.is_empty())
        };
        if Stage::HistoryRecords
            .batch(Duration::from_secs(10), batch)
            .await
            .map_err(history::Error::from)??
        {
            break;
        }
    }
    if !completions.is_empty() {
        return Err(Error::corrupt(
            "every number completion retains its original record",
        ));
    }
    Ok(states)
}
