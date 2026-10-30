//! Startup authenticates Grant continuity and each permanent command's original result.

use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Utc};
use tokio_postgres::Client;
use uuid::Uuid;

use crate::{
    access::{
        assignment,
        grant::{
            CommandError, command,
            record::{Event, Record},
            resumption,
            state::State,
            suspension,
            termination::{self, Mode},
        },
    },
    database::verification::Stage,
    history::{self, Kind as _, Read, Record as _},
    identity::{account, person::revision},
    kernel::{App, completion},
};

pub(super) async fn verify(
    client: &Client,
    app: &App,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<(), CommandError> {
    let states = history::inspect::<State>(client, app.keys(), now).await?;
    let states = states
        .into_iter()
        .map(|state| (*state.id().as_bytes(), state))
        .collect::<HashMap<_, _>>();
    let read = Read::Snapshot(app.anchors().view()?);
    let mut completions = HashMap::new();
    for (kind, template) in [
        ("grant", command::COMPLETIONS),
        ("assignment", assignment::operation::COMPLETIONS),
        ("suspension", suspension::COMPLETIONS),
        ("resumption", resumption::COMPLETIONS),
        ("termination", termination::COMPLETIONS),
    ] {
        for completion in completion::fetch_all(client, app.keys(), template).await? {
            completions.insert((kind, *completion.operation_id().as_bytes()), completion);
        }
    }
    let mut cursor = None;
    let mut sequence = 0_i64;
    let sql = format!(
        "{} WHERE $1::uuid IS NULL OR (record.grant_id,record.sequence)>($1,$2) ORDER BY record.grant_id,record.sequence LIMIT 128",
        Record::SELECT
    );
    loop {
        let batch = async {
            let rows = client
                .query(&sql, &[&cursor, &sequence])
                .await
                .map_err(CommandError::query)?;
            let records = rows
                .iter()
                .map(|row| history::record::<Record>(row, app.keys(), now))
                .collect::<Result<Vec<_>, _>>()?;
            let persons = records
                .iter()
                .filter_map(|record| match record.event() {
                    Event::Assignment {
                        previous_id: None, ..
                    } => Some(record.person_id()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let accounts =
                account::state::for_persons(client, app.keys(), app.anchors(), read, &persons, now)
                    .await?;
            let accounts = persons
                .into_iter()
                .map(|person| *person.as_bytes())
                .zip(accounts)
                .collect::<HashMap<_, _>>();
            let operators = records
                .iter()
                .filter_map(|record| match record.event() {
                    Event::Assignment {
                        operator_id,
                        revision_id,
                        previous_id: None,
                        ..
                    } => states.get(record.grant_id().as_bytes()).map(|state| {
                        if state.invitation_id().is_some() {
                            *revision_id
                        } else {
                            *operator_id
                        }
                    }),
                    Event::Assignment { operator_id, .. }
                    | Event::Suspension { operator_id, .. }
                    | Event::Resumption { operator_id, .. }
                    | Event::Termination {
                        operator_id,
                        cancellation_id: None,
                        ..
                    } => Some(*operator_id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let operators = revisions.fetch_identities(client, &operators).await?;
            for record in &records {
                cursor = Some(record.grant_id());
                sequence = record.sequence();
                let kind = match record.event() {
                    Event::Assignment {
                        previous_id: None, ..
                    } => "grant",
                    Event::Assignment {
                        previous_id: Some(_),
                        ..
                    } => "assignment",
                    Event::Suspension { .. } => "suspension",
                    Event::Resumption { .. } => "resumption",
                    Event::Termination {
                        cancellation_id: None,
                        ..
                    } => "termination",
                    _ => continue,
                };
                let completion = completions
                    .remove(&(kind, *record.operation_id().as_bytes()))
                    .ok_or(CommandError::corrupt(
                        "every permanent Grant command retains exactly one completion",
                    ))?;
                let state = states
                    .get(record.grant_id().as_bytes())
                    .ok_or(CommandError::corrupt("a Grant record retains its state"))?;
                let (action, target, operator_id) = match record.event() {
                    Event::Assignment {
                        previous_id: None,
                        operator_id,
                        revision_id,
                        ..
                    } => {
                        if state.invitation_id().is_some() {
                            ("accept", "invitations".to_owned(), *revision_id)
                        } else {
                            let account = accounts
                                .get(state.person_id().as_bytes())
                                .and_then(Option::as_ref)
                                .ok_or(CommandError::corrupt(
                                    "a direct admission retains its permanent account",
                                ))?;
                            (
                                "create",
                                Uuid::from_bytes(*account.id().as_bytes()).to_string(),
                                *operator_id,
                            )
                        }
                    }
                    Event::Assignment { operator_id, .. } => (
                        "assign",
                        Uuid::from_bytes(*state.id().as_bytes()).to_string(),
                        *operator_id,
                    ),
                    Event::Suspension { operator_id, .. } => (
                        "suspend",
                        Uuid::from_bytes(*state.id().as_bytes()).to_string(),
                        *operator_id,
                    ),
                    Event::Resumption {
                        id, operator_id, ..
                    } => (
                        "resume",
                        Uuid::from_bytes(*id.as_bytes()).to_string(),
                        *operator_id,
                    ),
                    Event::Termination {
                        mode, operator_id, ..
                    } => (
                        if *mode == Mode::Current {
                            "leave"
                        } else {
                            "terminate"
                        },
                        Uuid::from_bytes(*state.id().as_bytes()).to_string(),
                        *operator_id,
                    ),
                    Event::Loss { .. } => {
                        return Err(CommandError::corrupt(
                            "a role loss uses its cause's completion",
                        ));
                    }
                };
                let operator = operators.get(&operator_id).ok_or(CommandError::corrupt(
                    "a Grant command retains its original operator",
                ))?;
                if completion.action() != action
                    || completion.target() != target
                    || completion.created_at() != record.happened_at()
                    || completion.person_id() != operator.person_id()
                    || completion.revision_id() != operator_id
                {
                    return Err(CommandError::corrupt(
                        "a Grant completion agrees with its original actor, action, target and time",
                    ));
                }
            }
            Ok::<_, CommandError>(records.len())
        };
        let count = Stage::Grants
            .batch(Duration::from_secs(10), batch)
            .await
            .map_err(history::Error::from)??;
        if count == 0 {
            break;
        }
    }
    if !completions.is_empty() {
        return Err(CommandError::corrupt(
            "a Grant completion retains its original record",
        ));
    }
    history::initialize(app.anchors(), states.into_values())?;
    Ok(())
}
