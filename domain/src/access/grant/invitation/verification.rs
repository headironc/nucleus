//! Startup authenticates invitation continuity, completed inputs and admission pairing.

use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use super::{
    Code, Input, acceptance, adjustment, command, keys,
    record::{self, Event},
    state::State,
    withdrawal,
};
use crate::{
    access::{
        capability::Scope,
        grant::{self, CommandError, Rejection},
        role,
    },
    database::verification::Stage,
    history::{self, Record as _},
    identity::person::revision,
    kernel::{completion, operation::Family},
};

pub(super) async fn verify(
    client: &Client,
    roles: &role::Reader,
    keys: &keys::Keys,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<(), CommandError> {
    let states = history::inspect::<State>(client, roles.keys(), now).await?;
    for batch in states.chunks(128) {
        Stage::InvitationsBatch
            .batch(Duration::from_secs(10), async {
                let selections = batch
                    .iter()
                    .map(|state| (state.role_id(), state.change_id()))
                    .collect::<Vec<_>>();
                role::database::definitions(
                    client,
                    roles.keys(),
                    &selections,
                    Scope::Workforce,
                    now,
                )
                .await?;
                for state in batch {
                    let creator =
                        grant::state::find(client, roles, state.grant_id(), roles.read()?, now)
                            .await?
                            .ok_or(CommandError::corrupt(
                                "an invitation retains its original Grant",
                            ))?;
                    if creator.person_id() != state.person_id()
                        || creator.epoch() < state.epoch()
                        || creator.granted_at() > state.created_at()
                    {
                        return Err(CommandError::corrupt(
                            "an invitation retains its original creator and admission epoch",
                        ));
                    }
                    let code = Code::open(
                        keys,
                        state.id(),
                        state.grant_id(),
                        state.epoch(),
                        state.code(),
                    )?;
                    if code
                        .lookup(
                            keys.lookup
                                .fetch(state.key_id())
                                .map_err(grant::Error::from)?,
                        )
                        .map_err(grant::Error::from)?
                        != *state.hash()
                    {
                        return Err(CommandError::corrupt(
                            "an invitation lookup digest belongs to its original code",
                        ));
                    }
                }
                Ok::<_, CommandError>(())
            })
            .await
            .map_err(history::Error::from)??;
    }
    let states = states
        .into_iter()
        .map(|state| (*state.id().as_bytes(), state))
        .collect::<HashMap<_, _>>();
    let mut completions = HashMap::new();
    for (family, template) in [
        (Family::Invitation, command::COMPLETIONS),
        (Family::InvitationAdjustment, adjustment::COMPLETIONS),
        (Family::InvitationWithdrawal, withdrawal::COMPLETIONS),
        (Family::Grant, grant::command::COMPLETIONS),
    ] {
        for completion in completion::fetch_all(client, roles.keys(), template).await? {
            if family != Family::Grant || completion.action() == "accept" {
                completions.insert((family, *completion.operation_id().as_bytes()), completion);
            }
        }
    }
    let mut cursor = None;
    let mut sequence = 0;
    let mut previous: Option<history::Verified<record::Record>> = None;
    loop {
        let count = Stage::InvitationsBatch.batch(Duration::from_secs(10), async {
            let records = record::page(client, roles.keys(), cursor, sequence, now).await?;
            let count = records.len();
            for record in records {
                cursor = Some(record.invitation_id());
                sequence = record.sequence();
                let state = states.get(record.invitation_id().as_bytes())
                    .ok_or(CommandError::corrupt("an invitation record retains its state"))?;
                let operator = revisions.fetch_identity(client, record.operator_id()).await?;
                if operator.person_id() != state.person_id() {
                    return Err(CommandError::corrupt("an invitation record belongs to its creator's identity"));
                }
                let (family, action, target) = match record.event() {
                    Event::Creation => (Family::Invitation, "create", "invitations".to_owned()),
                    Event::Adjustment { .. } => (Family::InvitationAdjustment, "adjust", state.id().0.to_string()),
                    Event::Withdrawal => (Family::InvitationWithdrawal, "withdraw", state.id().0.to_string()),
                    Event::Acceptance { .. } => (Family::Grant, "accept", "invitations".to_owned()),
                };
                let completion = completions.remove(&(family, *record.operation_id().as_bytes()))
                    .ok_or(CommandError::corrupt("every invitation record retains one completion"))?;
                let (person_id, revision_id) = match record.event() {
                    Event::Creation => {
                        let input: Input = completion.input::<_, Rejection>(roles.keys())?;
                        if input.role.id != state.role_id() || input.role.change_id != state.change_id()
                            || input.capacity != state.original_capacity() || input.expires_at != state.original_expires_at() {
                            return Err(CommandError::corrupt("invitation creation agrees with its complete input"));
                        }
                        (operator.person_id(), operator.revision_id())
                    }
                    Event::Adjustment { .. } => {
                        let input: adjustment::Input = completion.input::<_, Rejection>(roles.keys())?;
                        let before = previous.as_ref().filter(|before| before.invitation_id() == state.id())
                            .ok_or(CommandError::corrupt("an adjustment retains its preceding record"))?;
                        if input.expected.capacity != before.capacity() || input.expected.expires_at != before.expires_at()
                            || input.capacity != record.capacity() || input.expires_at != record.expires_at() {
                            return Err(CommandError::corrupt("adjustment input agrees with its predecessor and result"));
                        }
                        (operator.person_id(), operator.revision_id())
                    }
                    Event::Withdrawal => {
                        completion.input::<(), Rejection>(roles.keys())?;
                        (operator.person_id(), operator.revision_id())
                    }
                    Event::Acceptance { grant_id } => {
                        record::check_acceptance_pair(client, *grant_id).await?;
                        let (grant, assignment) = grant::database::original(client, roles, record.operation_id(), revisions, now)
                            .await?.ok_or(CommandError::corrupt("an acceptance retains its original Grant"))?;
                        let admission = grant::state::find(client, roles, *grant_id, roles.read()?, now).await?
                            .ok_or(CommandError::corrupt("an acceptance retains its Grant state"))?;
                        let input: acceptance::Input = completion.input::<_, Rejection>(roles.keys())?;
                        let code = Code::open(keys, state.id(), state.grant_id(), state.epoch(), state.code())?;
                        if grant.id() != *grant_id || admission.invitation_id() != Some(state.id())
                            || grant.operator().revision_id() != record.operator_id() || grant.granted_at() != record.happened_at()
                            || assignment.role().id() != state.role_id() || assignment.role().change_id() != input.change_id
                            || !code.matches(&input.code) {
                            return Err(CommandError::corrupt("acceptance, Grant, first assignment and completed input agree"));
                        }
                        (grant.subject().person_id(), grant.subject().revision_id())
                    }
                };
                if completion.action() != action || completion.target() != target
                    || completion.created_at() != record.happened_at() || completion.person_id() != person_id
                    || completion.revision_id() != revision_id {
                    return Err(CommandError::corrupt("an invitation completion agrees with its original actor, action, target and time"));
                }
                previous = Some(record);
            }
            Ok::<_, CommandError>(count)
        }).await.map_err(history::Error::from)??;
        if count == 0 {
            break;
        }
    }
    if !completions.is_empty() {
        return Err(CommandError::corrupt(
            "every invitation completion retains its original record",
        ));
    }
    history::initialize(roles.anchors(), states.into_values())?;
    Ok(())
}
