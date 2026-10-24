//! Authenticated Grant state, history projections and original admissions.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, GenericClient};

use crate::{
    access::{
        assignment::{self, Assignment},
        grant::{Confirmation, Error, Filter, Grant, Id, List, member, state::State},
        role,
    },
    database::verification,
    history::{self, Kind as _, Verified},
    identity::{
        account, operation,
        person::{self, revision},
    },
};

pub(in crate::access) async fn verify(
    client: &impl GenericClient,
    accounts: &[Verified<account::state::State>],
    now: DateTime<Utc>,
) -> Result<(), crate::access::Error> {
    let persons = accounts
        .iter()
        .filter(|account| account.current().is_some())
        .map(|account| account.person_id())
        .collect::<Vec<_>>();
    let valid: bool = verification::Stage::GrantRelations.query(
        std::time::Duration::from_secs(10),
        client.query_one(
            "SELECT NOT EXISTS (SELECT 1 FROM access.grants WHERE termination IS NULL AND NOT (person_id=ANY($1))) AND NOT EXISTS (SELECT 1 FROM access.grants WHERE granted_at > $2 OR changed_at > $2)",
            &[&persons, &now],
        ),
    ).await??.try_get(0)?;
    if !valid {
        return Err(crate::access::Error::Facts {
            rule: "unfinished grants retain live accounts and Grant times are not in the future",
        });
    }
    Ok(())
}

pub(in crate::access) async fn lock_operator(
    client: &Client,
    account_id: account::Id,
) -> Result<(), Error> {
    super::state::lock_operator(client, account_id).await?;
    Ok(())
}

async fn check_accounts(
    client: &Client,
    roles: &role::Reader,
    states: &[Verified<State>],
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let persons = states
        .iter()
        .filter(|state| state.unfinished())
        .map(|state| state.person_id())
        .collect::<Vec<_>>();
    let accounts = account::state::for_persons(
        client,
        roles.keys(),
        roles.anchors(),
        roles.read()?,
        &persons,
        now,
    )
    .await?;
    if accounts.iter().any(|account| {
        !account
            .as_ref()
            .is_some_and(|account| account.current().is_some())
    }) {
        return Err(Error::Facts {
            rule: "an unfinished grant retains its authenticated live account",
        });
    }
    Ok(())
}

async fn for_person(
    client: &Client,
    roles: &role::Reader,
    person_id: person::Id,
    now: DateTime<Utc>,
) -> Result<Vec<Verified<State>>, Error> {
    let states = super::state::for_person(client, roles, person_id, roles.read()?, now).await?;
    check_accounts(client, roles, &states, now).await?;
    Ok(states)
}

fn project(
    state: &State,
    subject: revision::Identity,
    operator: revision::Identity,
) -> Result<Grant, Error> {
    if subject.person_id() != state.person_id() {
        return Err(Error::Facts {
            rule: "an admission retains its subject's fixed identity",
        });
    }
    Ok(Grant {
        id: state.id(),
        subject,
        operator,
        granted_at: state.granted_at(),
        sequence: 1,
        operation_id: state.operation_id(),
        status: state.status(),
    })
}

async fn deliver(
    client: &Client,
    state: &State,
    revisions: &revision::Reader,
) -> Result<Grant, Error> {
    project(
        state,
        revisions
            .fetch_identity(client, state.revision_id())
            .await?,
        revisions
            .fetch_identity(client, state.operator_id())
            .await?,
    )
}

pub(super) async fn fetch_list(
    client: &Client,
    roles: &role::Reader,
    filter: &Filter,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<List, Error> {
    let predicate = "($1::uuid IS NULL OR state.person_id=$1) AND ($2::uuid IS NULL OR state.id=$2) AND ($3::uuid IS NULL OR state.operation_id=$3)";
    let count = client
        .query_one(
            &format!("SELECT count(*) FROM access.grants state WHERE {predicate}"),
            &[&filter.person_id, &filter.grant_id, &filter.operation_id],
        )
        .await?
        .try_get(0)
        .map_err(Error::decode)?;
    let rows = client.query(&format!("{} WHERE {predicate} ORDER BY state.granted_at DESC,state.id DESC LIMIT $4 OFFSET $5", State::SELECT), &[&filter.person_id, &filter.grant_id, &filter.operation_id, &filter.pagination.size(), &filter.pagination.offset()]).await?;
    let states = rows
        .iter()
        .map(|row| history::state::<State>(row, roles.keys(), roles.anchors(), roles.read()?, now))
        .collect::<Result<Vec<_>, _>>()?;
    check_accounts(client, roles, &states, now).await?;
    let ids = states
        .iter()
        .flat_map(|state| [state.revision_id(), state.operator_id()])
        .collect::<Vec<_>>();
    let identities = revisions.fetch_identities(client, &ids).await?;
    let grants = states
        .iter()
        .map(|state| {
            let subject = identities
                .get(&state.revision_id())
                .ok_or(revision::ReadError::Missing)?
                .clone();
            let operator = identities
                .get(&state.operator_id())
                .ok_or(revision::ReadError::Missing)?
                .clone();
            project(state, subject, operator)
        })
        .collect::<Result<_, Error>>()?;
    Ok(List { count, grants })
}

pub(super) async fn find_history(
    client: &Client,
    roles: &role::Reader,
    id: Id,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<Option<(Grant, Assignment)>, Error> {
    let Some(state) = super::state::find(client, roles, id, roles.read()?, now).await? else {
        return Ok(None);
    };
    check_accounts(client, roles, std::slice::from_ref(&state), now).await?;
    let assignment = assignment::fetch_first(client, roles, id, revisions, now).await?;
    Ok(Some((
        deliver(client, &state, revisions).await?,
        assignment,
    )))
}

pub(super) async fn find_latest_by_person(
    client: &Client,
    roles: &role::Reader,
    person_id: person::Id,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<Option<Grant>, Error> {
    match for_person(client, roles, person_id, now).await?.first() {
        Some(state) => Ok(Some(deliver(client, state, revisions).await?)),
        None => Ok(None),
    }
}

pub(in crate::access) async fn find_by_person(
    client: &Client,
    roles: &role::Reader,
    person_id: person::Id,
    now: DateTime<Utc>,
) -> Result<Option<Confirmation>, Error> {
    Ok(for_person(client, roles, person_id, now)
        .await?
        .first()
        .map(|state| Confirmation {
            id: state.id(),
            status: state.status(),
        }))
}

pub(super) async fn find_role_by_person(
    client: &Client,
    roles: &role::Reader,
    person_id: person::Id,
    now: DateTime<Utc>,
) -> Result<Option<role::Role>, Error> {
    Ok(member::find_by_person(client, roles, person_id, now)
        .await?
        .map(member::Member::into_role))
}

pub(super) async fn original(
    client: &Client,
    roles: &role::Reader,
    operation_id: operation::Id,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<Option<(Grant, Assignment)>, Error> {
    let Some(row) = client
        .query_opt(
            &format!(
                "{} WHERE state.operation_id=$1 FOR SHARE OF state",
                State::SELECT
            ),
            &[&operation_id],
        )
        .await?
    else {
        return Ok(None);
    };
    let state = history::state::<State>(
        &row,
        roles.keys(),
        roles.anchors(),
        history::Read::Shared,
        now,
    )?;
    let assignment = assignment::fetch_first(client, roles, state.id(), revisions, now).await?;
    Ok(Some((
        deliver(client, &state, revisions).await?,
        assignment,
    )))
}
