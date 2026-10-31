//! Controlled offline correction of a system administrator's current affiliation.

use std::{sync::Arc, time::Duration};

use serde::Deserialize;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Client;

use crate::{
    access::{self, holder, role},
    center::affiliation::{Code, Id, command, database},
    database::transaction,
    history::{self, Read},
    identity::{
        account, keys,
        mobile::Mobile,
        person::{self, revision},
    },
    kernel::{self, App, Fault, FaultKind, Stage},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    operator: Operator,
    target: Target,
    affiliation: Code,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Operator {
    mobile: Mobile,
    account_id: account::Id,
    revision_id: revision::Id,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Target {
    account_id: account::Id,
    person_id: person::Id,
    revision_id: revision::Id,
    affiliation_id: Id,
}

impl Input {
    pub fn masked(&self) -> String {
        format!(
            "{}****{} / {} / {}",
            &self.operator.mobile.as_ref()[..3],
            &self.operator.mobile.as_ref()[7..],
            self.target.person_id,
            self.target.affiliation_id
        )
    }
}

pub type Error = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Operator,
    Target,
    TargetChanged,
    Unset,
    Unchanged,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Operator => kernel::Status::Unprocessable,
            Self::Target | Self::Unset => kernel::Status::NotFound,
            Self::TargetChanged | Self::Unchanged => kernel::Status::Conflict,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Operator => "operator_unavailable",
            Self::Target => "target_unavailable",
            Self::TargetChanged => "target_changed",
            Self::Unset => "affiliation_unset",
            Self::Unchanged => "state_conflict",
        }
    }
}

impl From<Rejection> for Error {
    fn from(rejection: Rejection) -> Self {
        Self::Rejected(rejection)
    }
}

pub(crate) async fn run(
    app: App,
    keys: Arc<keys::Keys>,
    input: Input,
    dry_run: bool,
) -> Result<Option<()>, Error> {
    app.registry().ready()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let lease = match timeout_at(deadline, app.context().connections().lease()).await {
        Ok(result) => result.map_err(Error::connection)?,
        Err(error) => return Error::elapsed(error),
    };
    let result = timeout_at(deadline, async {
        app.registry().ready()?;
        transaction::begin_current(app.context(), lease.client()).await?;
        correct(&app, keys, lease.client(), &input, dry_run).await
    })
    .await
    .unwrap_or_else(Error::elapsed);
    if result.as_ref().is_err_and(Error::is_corrupt) {
        app.registry().close();
    }
    command::finish(lease, deadline, result, dry_run).await?;
    Ok((!dry_run).then_some(()))
}

async fn correct(
    app: &App,
    keys: Arc<keys::Keys>,
    client: &Client,
    input: &Input,
    dry_run: bool,
) -> Result<(), Error> {
    transaction::shared(client, b"access/scope", b"institution").await?;
    let hash = keys.mobile.hash(&input.operator.mobile).map_err(crypto)?;
    let operator = account::state::discover_mobile(client, &hash).await?;
    let target = account::state::discover_person(client, input.target.person_id).await?;
    match (operator, target) {
        (Some(operator), Some(target)) => {
            account::state::lock_management(client, operator, target).await?
        }
        (Some(operator), None) => account::state::lock_shared(client, operator).await?,
        (None, Some(target)) => account::state::lock(client, target).await?,
        (None, None) => {}
    }
    let operator_person = match operator {
        Some(operator) => account::state::discover_owner(client, operator).await?,
        None => None,
    };
    person::state::lock_participants(client, operator_person, &[input.target.person_id]).await?;
    if operator != account::state::discover_mobile(client, &hash).await?
        || target != account::state::discover_person(client, input.target.person_id).await?
    {
        return Err(Error::lock_set_changed());
    }
    let now = app.context().now(client).await.map_err(Error::clock)?;
    let accounts = account::Reader::new(app.clone(), keys);
    let operator = accounts
        .find_by_id(
            client,
            operator.ok_or(Rejection::Operator)?,
            &input.operator.mobile,
            now,
        )
        .await?
        .ok_or(Rejection::Operator)?;
    if operator.suspension_id().is_some()
        || operator.id() != input.operator.account_id
        || operator.revision_id() != input.operator.revision_id
    {
        return Err(Rejection::Operator.into());
    }
    let target = target.ok_or(Rejection::Target)?;
    accounts
        .person_state(client, input.target.person_id, now)
        .await?
        .filter(|state| state.id() == target && state.current().is_some())
        .ok_or(Rejection::Target)?;
    let subject = accounts
        .find_current(client, target, now)
        .await?
        .ok_or(Rejection::Target)?;
    if subject.id() != input.target.account_id
        || subject.person_id() != input.target.person_id
        || subject.revision_id() != input.target.revision_id
    {
        return Err(Rejection::TargetChanged.into());
    }
    let holder = history::find::<holder::State>(
        client,
        app.keys(),
        app.anchors(),
        Read::Locked,
        target,
        now,
    )
    .await?
    .ok_or(Error::corrupt(
        "a current correction target retains its holder",
    ))?;
    if !holder.active() || holder.person_id() != input.target.person_id {
        return Err(Error::corrupt(
            "a current correction target and its holder agree",
        ));
    }
    let roles = role::Reader::new(app.clone());
    let role = roles
        .find(client, holder.role_id(), access::Scope::Institution, now)
        .await?
        .ok_or(Error::corrupt(
            "a current correction target retains its institutional role",
        ))?;
    if role.kind() != role::Kind::Full {
        return Err(Rejection::Target.into());
    }
    let latest = database::find_latest(client, input.target.person_id, now)
        .await?
        .ok_or(Rejection::Unset)?;
    if latest.id() != input.target.affiliation_id {
        return Err(Rejection::TargetChanged.into());
    }
    if latest.code(app.keys().data)? == input.affiliation {
        return Err(Rejection::Unchanged.into());
    }
    command::advancing(Some(latest.created_at()), now)?;
    if !dry_run {
        let id = Id::allocate();
        database::insert(
            client,
            app.keys().data,
            input.target.person_id,
            operator.revision_id(),
            id,
            input.affiliation,
            now,
        )
        .await?;
        database::audit(client, operator.person_id(), "system_correct", id).await?;
    }
    Ok(())
}

fn crypto(error: platform::cryptography::Error) -> Error {
    Error::Unavailable(Fault::new(Stage::Lock, FaultKind::Crypto(error)))
}
