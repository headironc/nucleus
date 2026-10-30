//! Workforce admission, permanent history, and the holder's grant and current role.

mod command;
pub(super) mod database;
pub mod invitation;
mod lifecycle;
pub mod member;
mod operation;
pub(in crate::access) mod record;
mod verification;
pub(crate) use record::{ASSIGNMENTS, LOSSES, RESUMPTIONS, SUSPENSIONS, TERMINATIONS};
pub(crate) use state::TABLE as GRANTS;
pub mod resumption;
pub(crate) mod state;
pub mod suspension;
pub mod termination;
pub(in crate::access) mod transition;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::AsRefStr;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;

pub use crate::access::grant::{
    command::{CommandError, Rejection},
    operation::{Input, Target},
};
use crate::{
    access::{Entry, appointment, assignment, capability::Scope, role},
    database::transaction,
    identity::{
        self, account,
        current::{self, Actor},
        keys,
        mobile::Mobile,
        person::{self, revision},
    },
    kernel::App,
    pagination::Pagination,
    runtime::{self, Context, registry::Registry},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = grant::Id, value_type = Uuid)]
pub struct Id(Uuid);

impl Id {
    fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(in crate::access) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl Entry {
    fn permission(self) -> role::Code {
        match self {
            Self::Domain => role::Code::WorkforceAdmission,
            Self::Institution => role::Code::InstitutionWorkforce,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, AsRefStr)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[schema(as = grant::Status)]
pub enum Status {
    Active,
    Suspended,
    Terminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = grant::Confirmation)]
pub struct Confirmation {
    id: Id,
    status: Status,
}

pub struct Candidate {
    identity: account::Subject,
    assignment_id: appointment::Id,
    suspension_id: Option<identity::suspension::Id>,
    grant: Option<Confirmation>,
    roles: role::List,
}

impl Candidate {
    pub fn identity(&self) -> &account::Subject {
        &self.identity
    }
    pub fn assignment_id(&self) -> appointment::Id {
        self.assignment_id
    }
    pub fn suspension_id(&self) -> Option<identity::suspension::Id> {
        self.suspension_id
    }
    pub fn grant(&self) -> Option<Confirmation> {
        self.grant
    }
    pub fn roles(&self) -> &[role::Role] {
        self.roles.roles()
    }
    pub fn count(&self) -> i64 {
        self.roles.count()
    }
}

pub struct Grant {
    id: Id,
    subject: revision::Identity,
    operator: revision::Identity,
    granted_at: DateTime<Utc>,
    sequence: i64,
    operation_id: crate::identity::operation::Id,
    status: Status,
}

impl Grant {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn subject(&self) -> &revision::Identity {
        &self.subject
    }
    pub fn operator(&self) -> &revision::Identity {
        &self.operator
    }
    pub fn granted_at(&self) -> DateTime<Utc> {
        self.granted_at
    }
    pub fn sequence(&self) -> i64 {
        self.sequence
    }
    pub fn operation_id(&self) -> crate::identity::operation::Id {
        self.operation_id
    }
    pub fn status(&self) -> Status {
        self.status
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Filter {
    #[serde(flatten)]
    pagination: Pagination,
    person_id: Option<person::Id>,
    grant_id: Option<Id>,
    operation_id: Option<identity::operation::Id>,
}

pub struct List {
    count: i64,
    grants: Vec<Grant>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }
}

pub(crate) struct Cancellation {
    pub(crate) id: crate::identity::cancellation::Id,
    pub(crate) operator_id: revision::Id,
    pub(crate) revision_id: revision::Id,
    pub(crate) operation_id: crate::identity::operation::Id,
    pub(crate) cancelled_at: DateTime<Utc>,
}

pub(crate) async fn cancel(
    client: &tokio_postgres::Client,
    roles: &role::Reader,
    person_id: person::Id,
    source: Cancellation,
    pending: &mut Option<crate::history::Pending>,
) -> Result<(), crate::history::Error> {
    state::lock_person(client, person_id).await?;
    if let Some(state) = state::current(
        client,
        roles,
        person_id,
        crate::history::Read::Locked,
        source.cancelled_at,
    )
    .await?
    {
        let candidate = transition::terminate(
            &state,
            termination::Mode::Cancellation,
            source.operator_id,
            source.revision_id,
            source.operation_id,
            Some(source.id),
            source.cancelled_at,
        )?
        .write(client, roles.keys(), roles.anchors())
        .await?;
        match pending {
            Some(pending) => pending.extend(candidate)?,
            None => *pending = Some(candidate),
        }
    }
    Ok(())
}

pub(crate) async fn confirm_cancellation(
    client: &tokio_postgres::Client,
    app: &App,
    person_id: person::Id,
    id: crate::identity::cancellation::Id,
    operation_id: crate::identity::operation::Id,
    read: crate::history::Read,
    now: DateTime<Utc>,
) -> Result<(), crate::history::Error> {
    use crate::history::Kind as _;
    let rows = client
        .query(
            &format!(
                "{} WHERE record.kind='termination' AND record.cancellation_id=$1",
                record::Record::SELECT
            ),
            &[&id],
        )
        .await?;
    for row in rows {
        let record = crate::history::record::<record::Record>(&row, app.keys(), now)?;
        if record.person_id() != person_id
            || crate::history::Record::operation_id(&*record) != operation_id
        {
            return Err(crate::history::Error::Integrity(
                "a cancellation retains exactly its Grant projections",
            ));
        }
        app.anchors().confirm::<state::State>(&record)?;
    }
    let roles = role::Reader::new(app.clone());
    if state::for_person(client, &roles, person_id, read, now)
        .await?
        .iter()
        .any(|state| state.unfinished())
    {
        return Err(crate::history::Error::Integrity(
            "a cancelled person retains no unfinished Grant",
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub struct Service {
    app: App,
    context: Context,
    roles: role::Reader,
    current: current::Service,
    invitations: Arc<invitation::keys::Keys>,
    accounts: account::Reader,
    revisions: revision::Reader,
    registry: Arc<Registry>,
}

pub(crate) async fn verify(
    client: &tokio_postgres::Client,
    app: &App,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    Ok(verification::verify(client, app, revisions, now).await?)
}

impl Service {
    pub(crate) fn prepare(
        app: App,
        current: current::Service,
        keys: Arc<keys::Keys>,
        invitations: invitation::keys::Keys,
    ) -> Self {
        Self {
            app: app.clone(),
            context: app.context().clone(),
            registry: Arc::clone(app.registry()),
            roles: role::Reader::new(app.clone()),
            current,
            accounts: account::Reader::new(app.clone(), keys),
            revisions: revision::Reader::new(app),
            invitations: Arc::new(invitations),
        }
    }

    #[cfg(test)]
    pub(crate) async fn verify(&self, client: &tokio_postgres::Client) -> Result<(), Error> {
        verify(
            client,
            &self.app,
            &self.revisions,
            self.current.now(client).await?,
        )
        .await
    }

    pub async fn list(&self, actor: &Actor, filter: Filter) -> Result<List, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let filter = &filter;
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    async move {
                        let (_, _, _, now) = member::authorize(
                            client,
                            &roles,
                            &self.current,
                            actor,
                            role::Code::WorkforceAdmission,
                        )
                        .await?;
                        database::fetch_list(client, &roles, filter, &self.revisions, now).await
                    }
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    pub async fn read(
        &self,
        actor: &Actor,
        id: Id,
    ) -> Result<(Grant, assignment::Assignment), Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    async move {
                        let (_, _, _, now) = member::authorize(
                            client,
                            &roles,
                            &self.current,
                            actor,
                            role::Code::WorkforceAdmission,
                        )
                        .await?;
                        database::find_history(client, &roles, id, &self.revisions, now)
                            .await?
                            .ok_or(Error::Absent)
                    }
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    pub async fn read_latest(&self, actor: &Actor) -> Result<Grant, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| async move {
                    let roles = self.roles.at(view);
                    let (person_id, _, now) =
                        self.current.at(view).fetch_person(client, actor).await?;
                    database::find_latest_by_person(client, &roles, person_id, &self.revisions, now)
                        .await?
                        .ok_or(Error::Missing)
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    pub async fn read_role(&self, actor: &Actor) -> Result<role::Role, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    async move {
                        let (person_id, _, now) =
                            self.current.at(view).fetch_person(client, actor).await?;
                        database::find_role_by_person(client, &roles, person_id, now)
                            .await?
                            .ok_or(Error::RoleAbsent)
                    }
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    pub async fn candidate(
        &self,
        actor: &Actor,
        entry: Entry,
        mobile: Mobile,
        pagination: Pagination,
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
                        let (role, person_id, _, now) =
                            self.authorize(client, &roles, actor, entry).await?;
                        let account_id = self
                            .accounts
                            .at(view)
                            .find_by_mobile(client, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        let target = appointment::find_by_account(client, &roles, account_id, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        Self::authorize_target(
                            client, &roles, entry, &role, person_id, &target, now,
                        )
                        .await?;
                        let identity = self
                            .accounts
                            .at(view)
                            .find_by_id(client, account_id, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        let suspension_id = identity.suspension_id();
                        let grant =
                            database::find_by_person(client, &roles, identity.person_id(), now)
                                .await?;
                        let roles = match entry {
                            Entry::Institution => {
                                role::database::fetch_list(
                                    client,
                                    &roles,
                                    pagination,
                                    Scope::Workforce,
                                    now,
                                )
                                .await?
                            }
                            Entry::Domain => {
                                role::database::fetch_assignable(
                                    client,
                                    &roles,
                                    pagination,
                                    Scope::Workforce,
                                    &role,
                                    now,
                                )
                                .await?
                            }
                        };
                        Ok(Candidate {
                            identity,
                            assignment_id: target.id(),
                            suspension_id,
                            grant,
                            roles,
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

    async fn finish<T>(
        &self,
        lease: platform::database::Lease,
        deadline: Instant,
        result: Result<T, Error>,
    ) -> Result<T, Error> {
        if let Err(cause) = &result
            && runtime::work::Failure::unreliable(cause)
        {
            self.registry.close();
        }
        match result {
            Ok(value) => {
                let (known, cleanup) =
                    transaction::confirm(value, deadline, lease.client().batch_execute("COMMIT"))
                        .await
                        .recycle(lease)
                        .await;
                match known.end {
                    Ok(()) => cleanup.map_err(Error::from).map(|()| known.value),
                    Err(error) => Err(Error::End(error).combine(cleanup)),
                }
            }
            Err(cause) if cause.rejected() => Err(Self::rollback(lease, cause, deadline).await),
            Err(cause) => Err(cause.combine(lease.discard().await)),
        }
    }
}

impl crate::database::read::Failure for Error {
    fn changed(&self) -> bool {
        crate::database::read::changed(self)
    }
}

impl From<crate::history::Error> for Error {
    fn from(error: crate::history::Error) -> Self {
        Self::Role(role::Error::History(error))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Command(CommandError),
    #[error("admission is denied")]
    Denied,
    #[error("grant input differs from the original operation")]
    Mismatch,
    #[error("the target is unavailable")]
    Absent,
    #[error("the person has no Workforce grant")]
    Missing,
    #[error("the person has no current Workforce role")]
    RoleAbsent,
    #[error("the confirmed target has changed")]
    TargetChanged,
    #[error("the selected role has changed")]
    RoleChanged,
    #[error("the account is not ready for admission")]
    AccountNotReady,
    #[error("the person already has an unterminated grant")]
    AlreadyAuthorized,
    #[error("the operation conflicts with the current state")]
    StateConflict,
    #[error("the invitation expiry is not later than the operation")]
    Expiry,
    #[error("the invitation capacity is below cumulative usage")]
    Capacity,
    #[error("the invitation terms are unchanged")]
    Unchanged,
    #[error("the invitation is unavailable")]
    InvitationUnavailable,
    #[error("{0}")]
    Current(#[from] current::Error),
    #[error("{0}")]
    Role(#[from] role::Error),
    #[error("{0}")]
    Appointment(#[from] appointment::Error),
    #[error("{0}")]
    Assignment(#[from] assignment::ReadError),
    #[error("{0}")]
    Account(#[from] account::ReadError),
    #[error("{0}")]
    Revision(#[from] revision::ReadError),
    #[error("{0}")]
    Suspension(#[from] identity::suspension::Error),
    #[error("{0}")]
    Cancellation(#[from] identity::cancellation::Error),
    #[error("grant fact check failed: {rule}")]
    Facts { rule: &'static str },
    #[error("grant query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("grant decoding failed: {0}")]
    Decode(#[source] crate::database::Query),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("{0}")]
    Work(#[from] runtime::work::Error),
    #[error("grant input or coordination encoding failed")]
    Crypto(#[from] platform::cryptography::Error),
    #[error("{0}")]
    Key(#[from] keys::request::Missing),
    #[error("{0}")]
    Invitation(#[from] invitation::keys::Error),
    #[error("{0}")]
    InvitationKey(#[from] invitation::keys::Missing),
    #[error("invitation code generation failed")]
    Random(#[from] getrandom::Error),
    #[error("grant transaction end failed: {0}")]
    End(#[source] transaction::EndError),
    #[error("original grant did not commit")]
    NotCommitted {
        #[source]
        cause: Box<Error>,
        commit: Result<(), transaction::EndError>,
        termination: Result<(), platform::database::Error>,
    },
    #[error("original grant outcome remains unknown")]
    Unknown {
        #[source]
        cause: Box<Error>,
        recovery: Box<Error>,
    },
    #[error("grant budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("{0}")]
    VerificationTimeout(#[from] crate::database::verification::Timeout),
    #[error("grant rejection rollback failed")]
    Rollback {
        #[source]
        cause: Box<Error>,
        rollback: transaction::EndError,
    },
    #[error("grant and connection termination failed")]
    Combined {
        #[source]
        cause: Box<Error>,
        termination: platform::database::Error,
    },
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(error.into())
    }
}

impl From<transaction::Error> for Error {
    fn from(error: transaction::Error) -> Self {
        match error {
            transaction::Error::Query(error) => Self::Query(error),
            transaction::Error::Crypto(error) => Self::Crypto(error),
            // The fence already closed admission; the outcome stays with the next process.
            transaction::Error::Superseded => Self::Work(runtime::work::Error::Closed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum Failure {
    Invalid,
    Denied,
    Mismatch,
    Absent,
    Missing,
    RoleAbsent,
    TargetChanged,
    RoleChanged,
    AccountNotReady,
    AlreadyAuthorized,
    StateConflict,
    Expiry,
    Capacity,
    Unchanged,
    InvitationUnavailable,
    Busy,
    Worker,
    Unavailable,
    Unknown,
}

impl Error {
    fn decode(error: tokio_postgres::Error) -> Self {
        Self::Decode(error.into())
    }

    fn rejected(&self) -> bool {
        matches!(
            self,
            Self::Denied
                | Self::Mismatch
                | Self::Absent
                | Self::Missing
                | Self::RoleAbsent
                | Self::TargetChanged
                | Self::RoleChanged
                | Self::AccountNotReady
                | Self::AlreadyAuthorized
                | Self::StateConflict
                | Self::Expiry
                | Self::Capacity
                | Self::Unchanged
                | Self::InvitationUnavailable
                | Self::Current(current::Error::Invalid)
        ) || matches!(
            self,
            Self::Role(role::Error::Denied | role::Error::Current(current::Error::Invalid))
        )
    }

    pub fn failure(&self) -> Failure {
        match self {
            Self::Command(crate::kernel::Error::Unknown(_)) => Failure::Unknown,
            Self::Denied | Self::Role(role::Error::Denied) => Failure::Denied,
            Self::Mismatch => Failure::Mismatch,
            Self::Absent => Failure::Absent,
            Self::Missing => Failure::Missing,
            Self::RoleAbsent => Failure::RoleAbsent,
            Self::TargetChanged => Failure::TargetChanged,
            Self::RoleChanged => Failure::RoleChanged,
            Self::AccountNotReady => Failure::AccountNotReady,
            Self::AlreadyAuthorized => Failure::AlreadyAuthorized,
            Self::StateConflict => Failure::StateConflict,
            Self::Expiry => Failure::Expiry,
            Self::Capacity => Failure::Capacity,
            Self::Unchanged => Failure::Unchanged,
            Self::InvitationUnavailable => Failure::InvitationUnavailable,
            Self::Current(current::Error::Invalid)
            | Self::Role(role::Error::Current(current::Error::Invalid)) => Failure::Invalid,
            Self::Work(runtime::work::Error::Busy) => Failure::Busy,
            Self::Work(runtime::work::Error::Worker) => Failure::Worker,
            Self::Unknown { .. } => Failure::Unknown,
            _ => Failure::Unavailable,
        }
    }

    fn combine(self, result: Result<(), platform::database::Error>) -> Self {
        match result {
            Ok(()) => self,
            Err(termination) => Self::Combined {
                cause: Box::new(self),
                termination,
            },
        }
    }
}

impl runtime::work::Failure for Error {
    fn unreliable(&self) -> bool {
        match self {
            Self::Command(error) => error.is_corrupt(),
            Self::Facts { .. }
            | Self::Decode(_)
            | Self::Key(_)
            | Self::Invitation(_)
            | Self::InvitationKey(_) => true,
            Self::Current(error) => error.unreliable(),
            Self::Role(error) => error.unreliable(),
            Self::Appointment(error) => error.unreliable(),
            Self::Assignment(error) => error.unreliable(),
            Self::Account(error) => error.unreliable(),
            Self::Revision(error) => error.unreliable(),
            Self::Suspension(error) => error.unreliable(),
            Self::Cancellation(error) => error.unreliable(),
            Self::Combined { cause, .. }
            | Self::Rollback { cause, .. }
            | Self::NotCommitted { cause, .. } => cause.unreliable(),
            Self::Unknown { cause, recovery } => cause.unreliable() || recovery.unreliable(),
            _ => false,
        }
    }

    fn report(&self, operation_id: crate::identity::operation::Id) {
        if !self.rejected() {
            tracing::error!(%operation_id, stage = "grant_completion", kind = self.failure().as_ref(), error = ?self);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use serde_json::{Value, json};
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use super::{Candidate, Entry, Error, Input, Status};
    use crate::{
        access::{
            appointment::tests::register,
            role::tests::{actor, start},
        },
        identity::{
            authentication::{Database, Supplier, server},
            operation,
        },
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "SQL fixtures name every part of the completion binding"
    )]
    pub(in crate::access) async fn fixture_completion(
        client: &tokio_postgres::Client,
        service: &super::Service,
        template: crate::kernel::completion::Template,
        action: &'static str,
        target: String,
        operator: crate::identity::person::revision::Id,
        operation: operation::Id,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        let person = client
            .query_one(
                "SELECT person_id FROM identity.revisions WHERE id=$1",
                &[&operator],
            )
            .await
            .expect("fixture operator")
            .get(0);
        let authority = crate::kernel::Authorization {
            person_id: person,
            revision_id: operator,
            now,
        };
        let request = crate::kernel::completion::Request::new::<std::convert::Infallible>(
            action,
            target,
            &json!({}),
            &authority,
        )
        .expect("fixture request");
        crate::kernel::completion::insert::<std::convert::Infallible>(
            client,
            service.app.keys(),
            template,
            operation,
            &request,
            now,
            None,
        )
        .await
        .expect("fixture completion");
    }

    pub(crate) async fn seed_admission(
        client: &tokio_postgres::Client,
        service: &super::Service,
        person: crate::identity::person::Id,
        operator: crate::identity::person::revision::Id,
        role_id: crate::access::role::Id,
        entry: Option<Entry>,
        invitation_id: Option<super::invitation::Id>,
    ) -> crate::history::Verified<super::state::State> {
        let now = client
            .query_one("SELECT observed_at FROM security.clocks WHERE id", &[])
            .await
            .expect("fixture time")
            .get(0);
        let subject = client.query_one("SELECT person.revision_id,account.id FROM identity.persons person JOIN identity.accounts account ON account.person_id=person.id WHERE person.id=$1", &[&person]).await.expect("fixture subject");
        let role = service
            .roles
            .find(
                client,
                role_id,
                crate::access::capability::Scope::Workforce,
                now,
            )
            .await
            .expect("fixture role read")
            .expect("fixture role");
        let operation = operation();
        let state = crate::history::Seed::write(
            client,
            service.app.keys(),
            super::transition::first(
                super::state::Admission {
                    id: super::Id::allocate(),
                    person_id: person,
                    operator_id: operator,
                    revision_id: subject.get("revision_id"),
                    entry,
                    invitation_id,
                    granted_at: now,
                    operation_id: operation,
                },
                &role,
                crate::access::assignment::Id::allocate(),
            ),
        )
        .await
        .expect("paired sealed admission");
        let (action, target) = if entry.is_some() {
            ("create", subject.get::<_, Uuid>("id").to_string())
        } else {
            ("accept", "invitations".to_owned())
        };
        fixture_completion(
            client,
            service,
            super::command::COMPLETIONS,
            action,
            target,
            operator,
            operation,
            now,
        )
        .await;
        state
    }

    #[derive(Clone, Copy)]
    pub(super) enum SqlAction {
        Suspend,
        Resume,
        SelfEnd,
        ManagerEnd,
    }

    async fn sql_admission(
        database: &Database,
        services: &crate::startup::Services,
        candidate: &Candidate,
        operator: crate::identity::person::revision::Id,
        entry: Entry,
    ) {
        use tokio_postgres::error::SqlState;

        use crate::history::{self, Seed};
        let service = services.grant();
        let writer = database
            .connections(&database.user)
            .recover()
            .await
            .expect("runtime admission writer");
        for case in [
            "valid",
            "raw control",
            "short seal",
            "missing seal",
            "missing key",
            "unregistered key",
            "state only",
            "record only",
            "missing completion",
            "initial sequence",
            "initial version",
            "first pairing",
            "wrong revision",
            "wrong scope",
            "wrong role",
            "duplicate",
        ] {
            writer
                .client()
                .batch_execute("BEGIN")
                .await
                .expect("isolated admission structure");
            let now = service.context.sample().await.expect("fixture time");
            let operation = operation();
            let admission = super::state::Admission {
                id: super::Id::allocate(),
                person_id: candidate.identity().person_id(),
                operator_id: operator,
                revision_id: candidate.identity().revision_id(),
                entry: Some(entry),
                invitation_id: None,
                granted_at: now,
                operation_id: operation,
            };
            let role = &candidate.roles()[0];
            let (state, record) = Seed::change(
                super::transition::first(
                    admission,
                    role,
                    crate::access::assignment::Id::allocate(),
                ),
                service.app.keys(),
            )
            .expect("sealed initial pair");
            let mut data = state.fixture_data();
            if case == "first pairing" {
                data.admission.operator_id = candidate.identity().revision_id();
            }
            if case == "wrong role" {
                data.role_id = candidate.roles()[1].id();
            }
            if case == "wrong revision" {
                data.admission.revision_id = operator;
            }
            if case == "wrong scope" {
                data.role_id = writer
                    .client()
                    .query_one(
                        "SELECT id FROM access.roles WHERE scope='institution' ORDER BY id LIMIT 1",
                        &[],
                    )
                    .await
                    .expect("real role from another scope")
                    .get(0);
            }
            let sequence = if case == "initial sequence" { 2 } else { 1 };
            let version = if case == "initial version" { 2 } else { 1 };
            let state =
                Seed::state::<super::state::State>(data, sequence, version, service.app.keys())
                    .expect("sealed admission variant");
            if case != "missing completion" {
                fixture_completion(
                    writer.client(),
                    service,
                    super::command::COMPLETIONS,
                    "create",
                    Uuid::from_bytes(*candidate.identity().id().as_bytes()).to_string(),
                    operator,
                    operation,
                    now,
                )
                .await;
            }
            let result: Result<(), history::Error> = async {
                if case != "state only" { Seed::persist_record(writer.client(), service.app.keys(), &record).await?; }
                if case == "duplicate" { Seed::persist_record(writer.client(), service.app.keys(), &record).await?; }
                if matches!(case, "raw control" | "short seal" | "missing seal" | "missing key" | "unregistered key") {
                    let tag = match case { "missing seal" => None, "short seal" => Some(vec![0_u8;31]), _ => Some(state.seal().tag().to_vec()) };
                    let key = match case { "missing key" => None, "unregistered key" => Some(crate::keys::Id::new(99).expect("key ID")), _ => Some(state.seal().key_id()) };
                    writer.client().execute("INSERT INTO access.grants(id,person_id,entry,operator_id,revision_id,granted_at,operation_id,role_id,assignment_id,epoch,changed_at,sequence,version,seal,seal_key_id,seal_version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)", &[&state.id(),&state.person_id(),&super::state::admission_label(state.entry()),&state.operator_id(),&state.revision_id(),&state.granted_at(),&state.operation_id(),&state.role_id(),&state.assignment_id(),&state.epoch(),&state.changed_at(),&sequence,&version,&tag,&key,&state.seal().version()]).await?;
                } else if case != "record only" { Seed::persist_state(writer.client(), &state, None).await?; }
                writer.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await?;
                Ok(())
            }.await;
            if matches!(case, "valid" | "raw control") {
                result.expect("complete admission control");
            } else {
                let error = result.expect_err("incomplete initial structure is rejected");
                let history::Error::Query(query) = &error else {
                    panic!("{case}: {error:?}");
                };
                let expected = match case {
                    "state only" | "record only" | "missing completion" | "wrong role"
                    | "unregistered key" | "wrong revision" | "wrong scope" => {
                        SqlState::FOREIGN_KEY_VIOLATION
                    }
                    "missing seal" | "missing key" => SqlState::NOT_NULL_VIOLATION,
                    "duplicate" => SqlState::UNIQUE_VIOLATION,
                    _ => SqlState::CHECK_VIOLATION,
                };
                assert_eq!(query.0.code(), Some(&expected), "{case}: {error:?}");
            }
            writer
                .client()
                .batch_execute("ROLLBACK")
                .await
                .expect("discard admission fixture");
        }
        writer.terminate().await.expect("admission writer ended");
    }

    pub(super) async fn sql_transition(
        database: &Database,
        services: &crate::startup::Services,
        grant_id: super::Id,
        operator: crate::identity::person::revision::Id,
        action: SqlAction,
    ) {
        use tokio_postgres::error::SqlState;

        use crate::history::{self, Read, Seed, State as _};
        let service = services.grant();
        let writer = database
            .connections(&database.user)
            .recover()
            .await
            .expect("runtime fixture writer");
        for case in [
            "valid",
            "raw control",
            "short seal",
            "missing seal",
            "missing key",
            "unregistered key",
            "record only",
            "state only",
            "missing completion",
            "head",
            "version",
            "sequence",
            "fixed admission",
            "assignment reference",
            "duplicate",
        ] {
            writer
                .client()
                .batch_execute("BEGIN")
                .await
                .expect("isolated structure case");
            let now = service.context.sample().await.expect("fixture time");
            let original = history::find::<super::state::State>(
                writer.client(),
                service.app.keys(),
                service.app.anchors(),
                Read::Locked,
                grant_id,
                now,
            )
            .await
            .expect("authenticated fixture state")
            .expect("fixture state");
            let operation = operation();
            let (change, template, label, target) = match action {
                SqlAction::Suspend => (
                    super::transition::suspend(
                        &original,
                        serde_json::from_value(json!(Uuid::new_v4())).expect("suspension ID"),
                        operator,
                        original.revision_id(),
                        operation,
                        now,
                    )
                    .expect("legal suspension"),
                    super::suspension::COMPLETIONS,
                    "suspend",
                    grant_id.0.to_string(),
                ),
                SqlAction::Resume => {
                    let id = original.suspension_id().expect("current suspension");
                    (
                        super::transition::resume(
                            &original,
                            id,
                            operator,
                            original.revision_id(),
                            operation,
                            now,
                        )
                        .expect("legal resumption"),
                        super::resumption::COMPLETIONS,
                        "resume",
                        Uuid::from_bytes(*id.as_bytes()).to_string(),
                    )
                }
                SqlAction::SelfEnd | SqlAction::ManagerEnd => {
                    let mode = if matches!(action, SqlAction::SelfEnd) {
                        super::termination::Mode::Current
                    } else {
                        super::termination::Mode::Manager
                    };
                    (
                        super::transition::terminate(
                            &original,
                            mode,
                            operator,
                            original.revision_id(),
                            operation,
                            None,
                            now,
                        )
                        .expect("legal termination"),
                        super::termination::COMPLETIONS,
                        if matches!(action, SqlAction::SelfEnd) {
                            "leave"
                        } else {
                            "terminate"
                        },
                        grant_id.0.to_string(),
                    )
                }
            };
            let (state, record) =
                Seed::change(change, service.app.keys()).expect("sealed fixture transition");
            let mut data = state.fixture_data();
            let mut sequence = state.sequence();
            let mut version = state.version();
            match case {
                "head" => data.changed_at += chrono::Duration::microseconds(1),
                "version" => version += 1,
                "sequence" => sequence = original.sequence(),
                "fixed admission" => data.admission.granted_at -= chrono::Duration::microseconds(1),
                "assignment reference" => {
                    data.assignment_id = writer.client().query_opt("SELECT id FROM access.assignments WHERE grant_id<>$1 ORDER BY id LIMIT 1", &[&grant_id]).await.expect("another retained Grant assignment").map(|row| row.get(0)).unwrap_or_else(crate::access::assignment::Id::allocate);
                }
                _ => (),
            }
            let state =
                Seed::state::<super::state::State>(data, sequence, version, service.app.keys())
                    .expect("sealed negative state");
            if case != "missing completion" {
                fixture_completion(
                    writer.client(),
                    service,
                    template,
                    label,
                    target,
                    operator,
                    operation,
                    now,
                )
                .await;
            }
            let result: Result<(), history::Error> = async {
                if matches!(case, "raw control" | "short seal" | "missing seal" | "missing key" | "unregistered key") {
                    let tag = match case { "missing seal" => None, "short seal" => Some(vec![0_u8;31]), _ => Some(record.seal().tag().to_vec()) };
                    let key = match case { "missing key" => None, "unregistered key" => Some(crate::keys::Id::new(99).expect("key ID")), _ => Some(record.seal().key_id()) };
                    match action {
                        SqlAction::Suspend | SqlAction::Resume => {
                            let (table,time,id) = if matches!(action, SqlAction::Suspend) { ("suspensions","suspended_at",state.suspension_id().expect("new suspension")) } else { ("resumptions","resumed_at",original.suspension_id().expect("old suspension")) };
                            writer.client().execute(&format!("INSERT INTO access.{table}(id,grant_id,person_id,role_id,assignment_id,operator_id,revision_id,{time},sequence,operation_id,epoch,seal,seal_key_id,seal_version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)"), &[&id,&grant_id,&original.person_id(),&original.role_id(),&original.assignment_id(),&operator,&original.revision_id(),&now,&state.sequence(),&operation,&state.epoch(),&tag,&key,&record.seal().version()]).await?;
                        }
                        SqlAction::SelfEnd | SqlAction::ManagerEnd => {
                            writer.client().execute("INSERT INTO access.terminations(id,person_id,role_id,assignment_id,suspension_id,operator_id,revision_id,terminated_at,sequence,operation_id,epoch,mode,seal,seal_key_id,seal_version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)", &[&grant_id,&original.person_id(),&original.role_id(),&original.assignment_id(),&original.suspension_id(),&operator,&original.revision_id(),&now,&state.sequence(),&operation,&state.epoch(),&super::state::termination_label(state.termination()),&tag,&key,&record.seal().version()]).await?;
                        }
                    }
                } else if case != "state only" { Seed::persist_record(writer.client(), service.app.keys(), &record).await?; }
                if case == "duplicate" { Seed::persist_record(writer.client(), service.app.keys(), &record).await?; }
                if case == "fixed admission" {
                    writer.client().execute("UPDATE access.grants SET granted_at=$2,role_id=$3,assignment_id=$4,suspension_id=$5,termination=$6,epoch=$7,changed_at=$8,sequence=$9,version=$10,seal=$11,seal_key_id=$12,seal_version=$13 WHERE id=$1", &[&grant_id,&state.granted_at(),&state.role_id(),&state.assignment_id(),&state.suspension_id(),&super::state::termination_label(state.termination()),&state.epoch(),&state.changed_at(),&state.sequence(),&state.version(),&state.seal().tag(),&state.seal().key_id(),&state.seal().version()]).await?;
                } else if case != "record only" { Seed::persist_state(writer.client(), &state, Some(&original)).await?; }
                writer.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await?;
                Ok(())
            }.await;
            if matches!(case, "valid" | "raw control" | "record only") {
                result.unwrap_or_else(|error| panic!("{case}: {error:?}"));
                if case == "record only" {
                    assert!(
                        history::inspect::<super::state::State>(
                            writer.client(),
                            service.app.keys(),
                            now
                        )
                        .await
                        .is_err(),
                        "startup rejects records beyond the retained state head"
                    );
                }
            } else {
                let error = result
                    .err()
                    .unwrap_or_else(|| panic!("{case}: invalid structure cannot commit"));
                let history::Error::Query(query) = &error else {
                    panic!("{case}: {error:?}");
                };
                let expected = match case {
                    "missing completion" | "assignment reference" | "unregistered key" => {
                        SqlState::FOREIGN_KEY_VIOLATION
                    }
                    "missing seal" | "missing key" => SqlState::NOT_NULL_VIOLATION,
                    "duplicate" => SqlState::UNIQUE_VIOLATION,
                    "state only" if matches!(action, SqlAction::Suspend) => {
                        SqlState::FOREIGN_KEY_VIOLATION
                    }
                    _ => SqlState::CHECK_VIOLATION,
                };
                assert_eq!(query.0.code(), Some(&expected), "{case}: {error:?}");
            }
            writer
                .client()
                .batch_execute("ROLLBACK")
                .await
                .expect("discard fixture facts");
            assert_eq!(writer.client().query_one("SELECT sequence AS person_sequence FROM identity.persons WHERE id=(SELECT person_id FROM access.grants WHERE id=$1)", &[&grant_id]).await.expect("independent personal order").get::<_,i64>(0), 1);
        }
        writer.terminate().await.expect("fixture writer ended");
    }

    fn input(candidate: &Candidate, index: usize) -> Value {
        let subject = candidate.identity();
        let kind = if index == 0 {
            crate::access::role::Kind::Default
        } else {
            crate::access::role::Kind::Full
        };
        let role = candidate
            .roles()
            .iter()
            .find(|role| role.kind() == kind)
            .expect("selected system role");
        json!({"entry":"institution","target":{
            "accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),
            "mobile":"13800000000","assignmentId":candidate.assignment_id(),
            "suspensionId":candidate.suspension_id(),"grant":candidate.grant()
        },"role":{"id":role.id(),"changeId":role.change_id()}})
    }

    fn command(value: &Value) -> Input {
        serde_json::from_value(value.clone()).expect("typed admission")
    }

    fn filter() -> super::Filter {
        serde_json::from_value(json!({})).expect("default history filter")
    }

    async fn termination_list(
        services: &crate::startup::Services,
        actor: &crate::identity::current::Actor,
        query: Value,
    ) -> Result<(i64, Vec<Value>), Error> {
        let page = services
            .grant()
            .list_terminations(actor, serde_json::from_value(query).expect("filter"))
            .await?;
        let identity = |value: &crate::identity::person::revision::Identity| {
            json!({
                "personId":value.person_id(),"revisionId":value.revision_id(),"name":value.name()
            })
        };
        Ok((page.count(), page.terminations().iter().map(|fact| json!({
            "id":fact.id(),"subject":identity(fact.subject()),"operator":identity(fact.operator()),
            "mode":fact.mode(),"terminatedAt":fact.terminated_at(),"sequence":fact.sequence(),"operationId":fact.operation_id()
        })).collect()))
    }

    #[test]
    fn termination_list_unites_all_modes_ranks_fixed_identities_filters_and_pages() {
        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (manager, reader, initial) = Box::pin(super::suspension::tests::participants(&database, &services, &mut supplier)).await;
                assert!(matches!(termination_list(&services, &reader, json!({})).await, Err(Error::Denied)));
                assert_eq!(termination_list(&services, &manager, json!({})).await.expect("empty"), (0, vec![]));
                Box::pin(services.grant().terminate(reader.clone(), operation(), initial.grant_id())).await.expect("self exit");
                let second = Box::pin(admit(&services, &manager, Entry::Domain, "13900000000", Kind::Default)).await;
                let candidate = services.grant().termination_candidate(&manager, "13900000000".parse().expect("mobile")).await.expect("end target");
                Box::pin(services.grant().terminate_member(manager.clone(), operation(), second.id(), serde_json::from_value(super::suspension::tests::confirmation(&candidate, "13900000000")).expect("end confirmation"))).await.expect("manager end");
                Box::pin(admit(&services, &manager, Entry::Domain, "13900000000", Kind::Default)).await;
                let mut role = Box::pin(services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"End reader","level":200,"permissions":["workforce.roles"]})).expect("role"))).await.expect("custom reader");
                Box::pin(assign_history_role(&services, &manager, Entry::Domain, "13900000000", Kind::Custom)).await;
                for permission in ["workforce.roles", "workforce.admission", "workforce.assignments", "workforce.suspension"] {
                    role = Box::pin(services.role().update_workforce(manager.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":format!("End reader {permission}"),"level":200,"permissions":[permission]})).expect("other permission"))).await.expect("role update");
                    assert!(matches!(termination_list(&services, &reader, json!({})).await, Err(Error::Denied)), "{permission}");
                }
                Box::pin(services.role().update_workforce(manager.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":"Exact end reader","level":200,"permissions":["workforce.termination"]})).expect("exact permission"))).await.expect("authorized reader");
                let candidate = services.grant().suspension_candidate(&manager, "13900000000".parse().expect("mobile")).await.expect("reader target");
                let body = super::suspension::tests::confirmation(&candidate, "13900000000");
                let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), candidate.grant_id(), serde_json::from_value(body.clone()).expect("pause"))).await.expect("reader paused");
                assert!(matches!(termination_list(&services, &reader, json!({})).await, Err(Error::Denied)));
                Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(body).expect("resume"))).await.expect("reader resumed");
                let full = services.grant().read_latest(&manager).await.expect("full member");
                Box::pin(services.grant().terminate(manager.clone(), operation(), full.id())).await.expect("full member exits");
                assert!(matches!(termination_list(&services, &manager, json!({})).await, Err(Error::Denied)));
                let cancelled = Box::pin(admit(&services, &reader, Entry::Institution, "13800000000", Kind::Full)).await;
                Box::pin(services.correction().correct(reader.clone(), operation(), serde_json::from_value(json!({"target":{"personId":full.subject().person_id(),"revisionId":full.subject().revision_id()},"name":"Changed Person","idNumber":"00000020000229007X"})).expect("correction"))).await.expect("later identity");
                let revised = services.current().read(&manager).await.expect("new identity");
                Box::pin(services.suspension().suspend(reader.clone(), operation(), revised.id(), serde_json::from_value(json!({"personId":revised.person_id(),"revisionId":revised.revision_id(),"mobile":"13800000000"})).expect("account pause"))).await.expect("account paused");
                assert_eq!(termination_list(&services, &manager, json!({})).await.expect_err("suspended account").failure(), super::Failure::Invalid);
                let cancellation_operation = operation();
                Box::pin(services.cancellation().cancel_account(reader.clone(), cancellation_operation, revised.id(), serde_json::from_value(json!({"personId":revised.person_id(),"revisionId":revised.revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("cancelled full member");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                // Independent persisted sources: no current account, role or admission operation participates.
                let rows = observer.client().query("SELECT id,person_id,revision_id,operator_id,terminated_at,sequence,operation_id,mode FROM access.terminations ORDER BY terminated_at DESC,id DESC", &[]).await.expect("independent persisted termination sources");
                let expected = rows.iter().map(|row| {
                    let person: crate::identity::person::Id = row.get("person_id");
                    let revision: crate::identity::person::revision::Id = row.get("revision_id");
                    let operator: crate::identity::person::revision::Id = row.get("operator_id");
                    json!({"id":row.get::<_,Uuid>("id"),"subject":{"personId":person,"revisionId":revision,"name":if revision==revised.revision_id(){"C*"}else if person==initial.identity().person_id(){"隔*"}else{"T*"}},
                        "operator":{"personId":if operator==initial.identity().revision_id(){initial.identity().person_id()}else{full.subject().person_id()},"revisionId":operator,"name":if operator==initial.identity().revision_id(){"隔*"}else{"T*"}},
                        "mode":row.get::<_,String>("mode"),"terminatedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("terminated_at"),"sequence":row.get::<_,i64>("sequence"),"operationId":row.get::<_,Uuid>("operation_id")})
                }).collect::<Vec<_>>();
                assert_eq!(expected.len(), 4);
                assert_eq!(termination_list(&services, &reader, json!({})).await.expect("all ranks and self"), (4, expected.clone()));
                for (index, item) in expected.iter().enumerate() {
                    for query in [json!({"page":index+1,"size":1}), json!({"grantId":item["id"]}), json!({"operationId":item["operationId"]}), json!({"personId":item["subject"]["personId"],"grantId":item["id"],"operationId":item["operationId"]})] {
                        let count = if query.get("page").is_some() {4} else {1};
                        assert_eq!(termination_list(&services, &reader, query).await.expect("page or intersection"), (count, vec![item.clone()]));
                    }
                }
                assert_eq!(expected[0]["id"], json!(cancelled.id()));
                assert_eq!(expected[0]["mode"], "cancellation");
                assert_eq!(expected[0]["operationId"], json!(cancellation_operation));
                assert_eq!(termination_list(&services, &reader, json!({"personId":initial.identity().person_id()})).await.expect("person filter"), (2, expected[2..].to_vec()));
                for query in [json!({"operationId":cancelled.operation_id()}), json!({"personId":initial.identity().person_id(),"grantId":cancelled.id()}), json!({"operationId":operation()})] {
                    assert_eq!(termination_list(&services, &reader, query).await.expect("unmatched operation or intersection"), (0, vec![]));
                }
                assert_eq!(termination_list(&services, &reader, json!({"page":i64::MAX,"size":1})).await.expect("past union"), (4, vec![]));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.terminations", &[]).await.expect("three manual ends and one cancellation projection").get::<_,i64>(0),4);
                services.ready().await.expect("denials preserve admission");
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded termination history scope");
        }));
    }

    #[test]
    fn termination_list_orders_equal_times_by_grant_id_across_the_union() {
        use crate::access::appointment::tests::{mobile, register_numbered};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database=Database::initialized(server).await;
                let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let (reader,target,initial)=Box::pin(super::suspension::tests::participants(&database,&services,&mut supplier)).await;
                let other=Box::pin(register_numbered(&database,&services,&mut supplier,10)).await;
                let grant=Box::pin(admit(&services,&reader,Entry::Domain,&mobile(10),crate::access::role::Kind::Default)).await;
                Box::pin(services.grant().terminate(target,operation(),initial.grant_id())).await.expect("self exit");
                let current=services.current().read(&other).await.expect("cancellation target");
                Box::pin(services.cancellation().cancel(other,operation(),serde_json::from_value(json!({"accountId":current.id(),"personId":current.person_id(),"revisionId":current.revision_id()})).expect("cancel input"))).await.expect("cancellation");
                let owner=database.connections("postgres").recover().await.expect("isolated owner");
                let at: chrono::DateTime<chrono::Utc>=owner.client().query_one("SELECT cancelled_at FROM identity.cancellations WHERE person_id=$1",&[&current.person_id()]).await.expect("committed cancellation time").get(0);
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("equal-time fixture");
                let manual = owner.client().query_one("SELECT operation_id,terminated_at FROM access.terminations WHERE id=$1", &[&initial.grant_id()]).await.expect("manual source");
                let manual_operation = manual.get("operation_id");
                let now = context.sample().await.expect("fixture time");
                super::record::retime(owner.client(), services.app().keys(), manual_operation, at, now).await.expect("sealed manual record");
                let state = super::state::retime(owner.client(), services.app(), initial.grant_id(), None, at, now).await.expect("paired current head");
                crate::kernel::completion::Seed::age(owner.client(), services.app().keys(), super::termination::COMPLETIONS, manual_operation, manual.get::<_,chrono::DateTime<chrono::Utc>>("terminated_at")-at).await.expect("paired completion time");
                owner.client().batch_execute("COMMIT").await.expect("fixture committed");
                crate::history::Seed::anchor(services.app().anchors(), &state).expect("fixture state anchor");
                let mut ids=[initial.grant_id().0,grant.id().0];
                ids.sort_by(|left,right|right.cmp(left));
                for (index,id) in ids.iter().enumerate() {
                    let (count,items)=termination_list(&services,&reader,json!({"page":index+1,"size":1})).await.expect("stable union page");
                    assert_eq!(count,2); assert_eq!(items.len(),1); assert_eq!(items[0]["id"],json!(id)); assert_eq!(items[0]["terminatedAt"],json!(at));
                    assert_eq!(items[0]["mode"],if *id==grant.id().0{"cancellation"}else{"self"});
                }
                // The largest offset the common pagination accepts must stay an empty page, not overflow.
                let (count,items)=termination_list(&services,&reader,json!({"page":4_611_686_018_427_387_904_i64,"size":2})).await.expect("extreme out-of-range page");
                assert_eq!((count,items.len()),(2,0));
                owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded union ordering");
        }));
    }

    #[test]
    fn termination_list_rejects_damage_and_closes_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["parent", "proof", "subject", "operator", "name", "future", "older", "cancellation parent", "cancellation missing", "cancellation sequence", "cancellation future", "cancellation mobile", "cancellation subject", "cancellation account", "cancellation sibling", "cancellation holder", "cancellation revision", "cancellation earlier", "orphan", "query"] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (reader, target, initial) = Box::pin(super::suspension::tests::participants(&database, &services, &mut supplier)).await;
                    let old = initial.grant_id();
                    Box::pin(services.grant().terminate(target.clone(), operation(), old)).await.expect("old end");
                    let grant = Box::pin(admit(&services, &reader, Entry::Domain, "13900000000", crate::access::role::Kind::Default)).await;
                    let derived = case.starts_with("cancellation");
                    let end_operation = operation();
                    if derived {
                        let current = services.current().read(&target).await.expect("target");
                        Box::pin(services.cancellation().cancel(target.clone(), end_operation, serde_json::from_value(json!({"accountId":current.id(),"personId":current.person_id(),"revisionId":current.revision_id()})).expect("cancel input"))).await.expect("real cancellation");
                    } else {
                        let candidate = services.grant().termination_candidate(&reader, "13900000000".parse().expect("mobile")).await.expect("target");
                        Box::pin(services.grant().terminate_member(reader.clone(), end_operation, grant.id(), serde_json::from_value(super::suspension::tests::confirmation(&candidate,"13900000000")).expect("end input"))).await.expect("real manager end");
                    }
                    let selected = || json!({"personId":grant.subject().person_id(),"grantId":grant.id(),"operationId":end_operation});
                    assert_eq!(termination_list(&services, &reader, selected()).await.expect("baseline").0, 1);
                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                    match case {
                        "parent" | "cancellation parent" => { owner.client().execute("DELETE FROM access.grants WHERE id=$1", &[&grant.id()]).await.expect("missing Grant"); }
                        "proof" => { owner.client().execute("UPDATE access.terminations SET seal_key_id=99 WHERE id=$1", &[&grant.id()]).await.expect("broken proof key"); }
                        "subject" => { owner.client().execute("UPDATE access.terminations SET revision_id=gen_random_uuid() WHERE id=$1", &[&grant.id()]).await.expect("missing identity"); }
                        "operator" => { owner.client().execute("UPDATE access.terminations SET operator_id=gen_random_uuid() WHERE id=$1", &[&grant.id()]).await.expect("missing operator"); }
                        "name" => { owner.client().execute("UPDATE identity.revisions SET name=id_number WHERE id=$1", &[&grant.subject().revision_id()]).await.expect("broken envelope"); }
                        "future" => { owner.client().execute("UPDATE access.terminations SET terminated_at=clock_timestamp()+interval '1 hour' WHERE id=$1", &[&grant.id()]).await.expect("future end"); }
                        "older" => { owner.client().execute("UPDATE access.terminations SET terminated_at=clock_timestamp()+interval '1 hour' WHERE id=$1", &[&old]).await.expect("damaged page-external end"); }
                        "cancellation missing" => { owner.client().execute("DELETE FROM identity.cancellations WHERE person_id=$1", &[&grant.subject().person_id()]).await.expect("missing end source"); }
                        "cancellation sequence" => { owner.client().execute("UPDATE identity.cancellations SET sequence=sequence+100 WHERE person_id=$1", &[&grant.subject().person_id()]).await.expect("bad sequence"); }
                        "cancellation future" => { owner.client().execute("UPDATE identity.cancellations SET cancelled_at=clock_timestamp()+interval '1 hour' WHERE person_id=$1", &[&grant.subject().person_id()]).await.expect("future cancellation"); }
                        "cancellation mobile" => { owner.client().execute("UPDATE identity.cancellations SET mobile=set_byte(mobile,octet_length(mobile)-1,get_byte(mobile,octet_length(mobile)-1)#1) WHERE person_id=$1", &[&grant.subject().person_id()]).await.expect("broken cancellation proof"); }
                        "cancellation subject" => { owner.client().execute("UPDATE identity.cancellations SET revision_id=gen_random_uuid() WHERE person_id=$1", &[&grant.subject().person_id()]).await.expect("missing cancellation identity"); }
                        "cancellation account" => { owner.client().execute("UPDATE identity.cancellations SET account_id=$1 WHERE person_id=$2", &[&reader.account_id(), &grant.subject().person_id()]).await.expect("inconsistent removed account"); }
                        // Filtered to the older manual end, the person's later cancellation must still be authenticated.
                        "cancellation sibling" => { owner.client().execute("UPDATE identity.cancellations SET mobile=set_byte(mobile,octet_length(mobile)-1,get_byte(mobile,octet_length(mobile)-1)#1) WHERE person_id=$1", &[&grant.subject().person_id()]).await.expect("broken sibling cancellation"); }
                        // Cancellation retains a tombstone, never an active holder.
                        "cancellation holder" => { owner.client().execute("UPDATE access.holders SET cancellation_id=NULL WHERE person_id=$1", &[&grant.subject().person_id()]).await.expect("reactivated cancelled holder"); }
                        // The cancellation names another person's real revision as its subject.
                        "cancellation revision" => { owner.client().execute("UPDATE identity.cancellations SET revision_id=$1 WHERE person_id=$2", &[&services.current().read(&reader).await.expect("reader").revision_id(), &grant.subject().person_id()]).await.expect("foreign cancellation subject"); }
                        // The earlier Grant loses its own end; the later cancellation must not replace it.
                        "cancellation earlier" => { owner.client().execute("DELETE FROM access.terminations WHERE id=$1", &[&old]).await.expect("missing earlier end"); }
                        // Both the Grant and its manual end disappear; only the retained assignment remains.
                        "orphan" => { owner.client().execute("DELETE FROM access.terminations WHERE id=$1", &[&grant.id()]).await.expect("missing manual end"); owner.client().execute("DELETE FROM access.grants WHERE id=$1", &[&grant.id()]).await.expect("missing Grant"); }
                        "query" => { owner.client().batch_execute(&format!("REVOKE SELECT ON access.terminations FROM {}", database.user)).await.expect("query failure"); }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let mut query = selected();
                    if matches!(case, "parent" | "cancellation parent") { query["page"]=json!(2); query["size"]=json!(1); }
                    if matches!(case, "cancellation sibling" | "cancellation revision") { query=json!({"grantId":old}); }
                    if case=="cancellation earlier" { query=json!({}); }
                    if case == "cancellation holder" {
                        assert_eq!(termination_list(&services, &reader, query).await.expect("the retained cancellation still defines the Grant end").0, 1);
                        assert!(matches!(
                            crate::access::verification::verify(owner.client(), services.app(), context.sample().await.expect("trusted time")).await,
                            Err(crate::access::Error::History(crate::history::Error::Seal(crate::keys::seal::Error::Mismatch))),
                        ), "the direct verifier rejects the changed Holder seal");
                        services.ready().await.expect("this historical read does not consume the cancelled Holder state");
                        owner.terminate().await.expect("owner ended");
                        services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                        assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                        return;
                    }
                    if matches!(case, "parent" | "cancellation parent" | "older" | "cancellation earlier" | "orphan") {
                        let page = termination_list(&services, &reader, query).await.expect("classification authenticates only returned records and fixed identities");
                        assert_eq!(page.0, i64::from(case != "orphan"), "{case}");
                        assert_eq!(page.1.len(), usize::from(!matches!(case, "parent" | "cancellation parent" | "orphan")), "{case}");
                        services.ready().await.expect("unread facts do not close admission");
                        assert!(services.grant().verify(owner.client()).await.is_err(), "startup rejects {case}");
                        owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                        assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                        return;
                    }
                    let error = termination_list(&services, &reader, query).await.expect_err("consumed damaged facts cannot become a result");
                    assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                    assert_eq!(crate::runtime::work::Failure::unreliable(&error), case!="query", "{case}: {error:?}");
                    assert_eq!(services.ready().await.is_err(), case!="query", "{case}");
                    if case=="query" {
                        owner.client().batch_execute(&format!("GRANT SELECT ON access.terminations TO {}",database.user)).await.expect("restore test access");
                        assert_eq!(termination_list(&services, &reader, selected()).await.expect("fresh page").0,1);
                    }
                    owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded termination integrity case");
            }
        }));
    }

    #[derive(Clone, Copy)]
    enum History {
        Suspensions,
        Resumptions,
        Terminations,
    }

    impl History {
        async fn list(
            self,
            services: &crate::startup::Services,
            actor: &crate::identity::current::Actor,
            query: Value,
        ) -> Result<(i64, Vec<Value>), Error> {
            if matches!(self, Self::Terminations) {
                return termination_list(services, actor, query).await;
            }
            let filter = serde_json::from_value(query).expect("history filter");
            let identity = |value: &crate::identity::person::revision::Identity| {
                json!({
                    "personId":value.person_id(),"revisionId":value.revision_id(),"name":value.name()
                })
            };
            match self {
                Self::Terminations => unreachable!(),
                Self::Suspensions => {
                    let page = services.grant().list_suspensions(actor, filter).await?;
                    Ok((page.count(), page.suspensions().iter().map(|fact| json!({
                        "id":fact.id(),"grantId":fact.grant_id(),"subject":identity(fact.subject()),
                        "operator":identity(fact.operator()),"at":fact.suspended_at(),"sequence":fact.sequence(),"operationId":fact.operation_id()
                    })).collect()))
                }
                Self::Resumptions => {
                    let page = services.grant().list_resumptions(actor, filter).await?;
                    Ok((page.count(), page.resumptions().iter().map(|fact| json!({
                        "id":fact.id(),"grantId":fact.grant_id(),"subject":identity(fact.subject()),
                        "operator":identity(fact.operator()),"at":fact.resumed_at(),"sequence":fact.sequence(),"operationId":fact.operation_id()
                    })).collect()))
                }
            }
        }

        async fn facts(
            self,
            client: &tokio_postgres::Client,
            initialized: crate::identity::person::Id,
        ) -> Vec<Value> {
            if matches!(self, Self::Terminations) {
                return client.query("SELECT termination.id,termination.person_id,termination.revision_id,termination.operator_id,operator.person_id AS operator_person,termination.terminated_at,termination.sequence,termination.operation_id FROM access.terminations termination JOIN identity.revisions operator ON operator.id=termination.operator_id ORDER BY termination.terminated_at DESC,termination.id DESC", &[]).await.expect("independent ends").iter().map(|row|json!({
                    "id":row.get::<_,Uuid>("id"),"subject":{"personId":row.get::<_,Uuid>("person_id"),"revisionId":row.get::<_,Uuid>("revision_id"),"name":if row.get::<_,crate::identity::person::Id>("person_id")==initialized {"隔*"}else{"T*"}},
                    "operator":{"personId":row.get::<_,Uuid>("operator_person"),"revisionId":row.get::<_,Uuid>("operator_id"),"name":if row.get::<_,crate::identity::person::Id>("operator_person")==initialized {"隔*"}else{"T*"}},
                    "mode":if row.get::<_,Uuid>("revision_id")==row.get::<_,Uuid>("operator_id") {"self"}else{"manager"},"terminatedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("terminated_at"),"sequence":row.get::<_,i64>("sequence"),"operationId":row.get::<_,Uuid>("operation_id")
                })).collect();
            }
            let (table, at, join, grant) = match self {
                Self::Terminations => unreachable!(),
                Self::Suspensions => ("suspensions", "suspended_at", "", "fact.grant_id"),
                Self::Resumptions => (
                    "resumptions",
                    "resumed_at",
                    "JOIN access.suspensions suspension ON suspension.id=fact.id",
                    "suspension.grant_id",
                ),
            };
            client.query(&format!("SELECT fact.id, {grant} AS grant_id, fact.{at} AS happened_at, fact.sequence, fact.operation_id, subject.person_id, subject.id AS revision_id, operator.person_id AS operator_person, operator.id AS operator_id FROM access.{table} fact {join} JOIN identity.revisions subject ON subject.id=fact.revision_id JOIN identity.revisions operator ON operator.id=fact.operator_id ORDER BY fact.{at} DESC, fact.id DESC"), &[]).await.expect("independent permanent state facts")
                .iter().map(|row| json!({
                    "id":row.get::<_,Uuid>("id"),"grantId":row.get::<_,Uuid>("grant_id"),
                    "at":row.get::<_,chrono::DateTime<chrono::Utc>>("happened_at"),"sequence":row.get::<_,i64>("sequence"),"operationId":row.get::<_,Uuid>("operation_id"),
                    "subject":{"personId":row.get::<_,Uuid>("person_id"),"revisionId":row.get::<_,Uuid>("revision_id"),"name":if row.get::<_,crate::identity::person::Id>("person_id")==initialized {"隔*"} else {"T*"}},
                    "operator":{"personId":row.get::<_,Uuid>("operator_person"),"revisionId":row.get::<_,Uuid>("operator_id"),"name":if row.get::<_,crate::identity::person::Id>("operator_person")==initialized {"隔*"} else {"T*"}}
                })).collect()
        }

        fn permission(self) -> &'static str {
            match self {
                Self::Terminations => "workforce.termination",
                Self::Suspensions | Self::Resumptions => "workforce.suspension",
            }
        }

        async fn cycle(
            self,
            services: &crate::startup::Services,
            manager: &crate::identity::current::Actor,
            target: &crate::identity::current::Actor,
        ) {
            if matches!(self, Self::Terminations) {
                let grant = services
                    .grant()
                    .read_latest(target)
                    .await
                    .expect("target grant");
                Box::pin(
                    services
                        .grant()
                        .terminate(target.clone(), operation(), grant.id()),
                )
                .await
                .expect("real exit");
                Box::pin(admit(
                    services,
                    manager,
                    Entry::Domain,
                    "13900000000",
                    crate::access::role::Kind::Default,
                ))
                .await;
            } else {
                Box::pin(state_cycle(services, manager, "13900000000")).await;
            }
        }
    }

    async fn state_cycle(
        services: &crate::startup::Services,
        manager: &crate::identity::current::Actor,
        mobile: &str,
    ) -> super::suspension::Id {
        let candidate = services
            .grant()
            .suspension_candidate(manager, mobile.parse().expect("mobile"))
            .await
            .expect("state target");
        let input = super::suspension::tests::confirmation(&candidate, mobile);
        let pause = Box::pin(services.grant().suspend(
            manager.clone(),
            operation(),
            candidate.grant_id(),
            serde_json::from_value(input.clone()).expect("suspension"),
        ))
        .await
        .expect("real suspension");
        Box::pin(services.grant().resume(
            manager.clone(),
            operation(),
            pause.id(),
            serde_json::from_value(input).expect("resumption"),
        ))
        .await
        .expect("real resumption");
        pause.id()
    }

    async fn assign_history_role(
        services: &crate::startup::Services,
        manager: &crate::identity::current::Actor,
        entry: Entry,
        mobile: &str,
        kind: crate::access::role::Kind,
    ) {
        let candidate = services
            .assignment()
            .candidate(
                manager,
                entry,
                mobile.parse().expect("mobile"),
                crate::pagination::Pagination::new(1, 100).expect("page"),
            )
            .await
            .expect("assignment target");
        let role = candidate
            .roles()
            .iter()
            .find(|role| role.kind() == kind)
            .expect("selected role");
        let subject = candidate.identity();
        Box::pin(services.assignment().assign(manager.clone(), operation(), serde_json::from_value(json!({
            "entry":entry.as_ref(),"target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),
            "mobile":mobile,"grantId":candidate.grant_id(),"assignmentId":candidate.assignment_id()},"role":{"id":role.id(),"changeId":role.change_id()}
        })).expect("assignment"))).await.expect("real role change");
    }

    fn state_history_scope(history: History) {
        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (manager, reader, initial) = Box::pin(super::suspension::tests::participants(&database, &services, &mut supplier)).await;
                assert!(matches!(history.list(&services, &reader, json!({})).await, Err(Error::Denied)), "institution alone cannot read");
                assert_eq!(history.list(&services, &manager, json!({})).await.expect("empty history"), (0, vec![]));
                Box::pin(state_cycle(&services, &manager, "13900000000")).await;
                let mut role = Box::pin(services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"State reader","level":200,"permissions":["workforce.roles"]})).expect("role"))).await.expect("reader role");
                Box::pin(assign_history_role(&services, &manager, Entry::Domain, "13900000000", Kind::Custom)).await;
                for permission in ["workforce.roles", "workforce.admission", "workforce.assignments"] {
                    role = Box::pin(services.role().update_workforce(manager.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":format!("Reader {permission}"),"level":200,"permissions":[permission]})).expect("other permission"))).await.expect("different authority");
                    assert!(matches!(history.list(&services, &reader, json!({})).await, Err(Error::Denied)), "{permission} cannot read");
                }
                Box::pin(services.role().update_workforce(manager.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":"Exact state reader","level":200,"permissions":["workforce.suspension"]})).expect("exact permission"))).await.expect("reader authorized");
                Box::pin(assign_history_role(&services, &reader, Entry::Institution, "13800000000", Kind::Default)).await;
                let first = services.grant().read_latest(&manager).await.expect("original manager grant");
                Box::pin(state_cycle(&services, &reader, "13800000000")).await;
                Box::pin(assign_history_role(&services, &reader, Entry::Institution, "13800000000", Kind::Full)).await;
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let expected = history.facts(observer.client(), initial.identity().person_id()).await;
                assert_eq!(expected.len(), 2);
                assert_eq!(history.list(&services, &reader, json!({})).await.expect("lower reader sees higher member and self"), (2, expected.clone()));
                for (query, count, values) in [
                    (json!({"page":1,"size":1}), 2, vec![expected[0].clone()]),
                    (json!({"page":2,"size":1}), 2, vec![expected[1].clone()]),
                    (json!({"page":i64::MAX,"size":1}), 2, vec![]),
                    (json!({"size":100,"personId":expected[0]["subject"]["personId"]}), 1, vec![expected[0].clone()]),
                    (json!({"grantId":expected[0]["grantId"]}), 1, vec![expected[0].clone()]),
                    (json!({"operationId":expected[0]["operationId"]}), 1, vec![expected[0].clone()]),
                    (json!({"personId":expected[0]["subject"]["personId"],"grantId":expected[0]["grantId"],"operationId":expected[0]["operationId"]}), 1, vec![expected[0].clone()]),
                    (json!({"personId":expected[1]["subject"]["personId"],"grantId":expected[0]["grantId"]}), 0, vec![]),
                    (json!({"operationId":operation()}), 0, vec![]),
                ] {
                    assert_eq!(history.list(&services, &reader, query).await.expect("filtered history"), (count, values));
                }
                let candidate = services.grant().suspension_candidate(&manager, "13900000000".parse().expect("mobile")).await.expect("reader target");
                let input = super::suspension::tests::confirmation(&candidate, "13900000000");
                let paused = Box::pin(services.grant().suspend(manager.clone(), operation(), candidate.grant_id(), serde_json::from_value(input.clone()).expect("pause reader"))).await.expect("reader suspended");
                assert!(matches!(history.list(&services, &reader, json!({})).await, Err(Error::Denied)));
                Box::pin(services.grant().resume(manager.clone(), operation(), paused.id(), serde_json::from_value(input).expect("resume reader"))).await.expect("reader restored");
                let expected = history.facts(observer.client(), initial.identity().person_id()).await;
                assert_eq!(expected.len(), 3);
                Box::pin(services.correction().correct(reader.clone(), operation(), serde_json::from_value(json!({"target":{"personId":first.subject().person_id(),"revisionId":first.subject().revision_id()},"name":"Changed Person","idNumber":"00000020000229007X"})).expect("correction"))).await.expect("later fixed identity version");
                Box::pin(services.grant().terminate(manager.clone(), operation(), first.id())).await.expect("real exit");
                assert!(matches!(history.list(&services, &manager, json!({})).await, Err(Error::Denied)), "former member denied");
                Box::pin(admit(&services, &reader, Entry::Institution, "13800000000", Kind::Full)).await;
                let revised = services.current().read(&manager).await.expect("current target");
                Box::pin(services.cancellation().cancel_account(reader.clone(), operation(), revised.id(), serde_json::from_value(json!({"personId":revised.person_id(),"revisionId":revised.revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("target cancelled");
                assert_eq!(history.list(&services, &reader, json!({})).await.expect("ended and cancelled facts retain both identities"), (3, expected));
                assert_eq!(history.list(&services, &manager, json!({})).await.expect_err("cancelled reader invalid").failure(), super::Failure::Invalid);
                services.ready().await.expect("denials preserve readiness");
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded state history scope");
        }));
    }

    #[test]
    fn suspension_list_preserves_scope_filters_pagination_and_fixed_identities() {
        state_history_scope(History::Suspensions);
    }

    #[test]
    fn resumption_list_preserves_scope_filters_pagination_and_fixed_identities() {
        state_history_scope(History::Resumptions);
    }

    fn state_history_integrity(history: History) {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["parent", "parent page", "proof", "subject", "operator", "name", "id_number", "future", "older", "query"] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (reader, target, candidate) = Box::pin(super::suspension::tests::participants(&database, &services, &mut supplier)).await;
                    let old = candidate.grant_id();
                    Box::pin(services.grant().terminate(target.clone(), operation(), old)).await.expect("retained old end");
                    let grant = Box::pin(admit(&services, &reader, Entry::Domain, "13900000000", crate::access::role::Kind::Default)).await;
                    let id = Box::pin(state_cycle(&services, &reader, "13900000000")).await;
                    let selected = || json!({"grantId":grant.id()});
                    assert_eq!(history.list(&services, &reader, selected()).await.expect("baseline").0, 1);
                    let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                    let (table, at) = match history { History::Suspensions => ("suspensions","suspended_at"), History::Resumptions => ("resumptions","resumed_at"), History::Terminations => unreachable!() };
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated post-startup damage");
                    match case {
                        "parent" | "parent page" => {
                            match history {
                                History::Suspensions => { owner.client().execute("DELETE FROM access.grants WHERE id=$1", &[&grant.id()]).await.expect("missing grant"); }
                                History::Resumptions => { owner.client().execute("DELETE FROM access.suspensions WHERE id=$1", &[&id]).await.expect("missing suspension"); }
                                History::Terminations => unreachable!(),
                            }
                        }
                        "proof" => { owner.client().execute(&format!("UPDATE access.{table} SET seal_key_id=99 WHERE id=$1"), &[&id]).await.expect("missing key"); }
                        "subject" => { owner.client().execute(&format!("UPDATE access.{table} SET revision_id=gen_random_uuid() WHERE id=$1"), &[&id]).await.expect("missing subject"); }
                        "operator" => { owner.client().execute(&format!("UPDATE access.{table} SET operator_id=gen_random_uuid() WHERE id=$1"), &[&id]).await.expect("missing operator"); }
                        "name" => { owner.client().execute("UPDATE identity.revisions SET name=id_number WHERE id=$1", &[&grant.subject().revision_id()]).await.expect("wrong envelope binding"); }
                        "id_number" => { owner.client().execute("UPDATE identity.revisions SET id_number=set_byte(id_number,octet_length(id_number)-1,get_byte(id_number,octet_length(id_number)-1)#1) WHERE id=$1", &[&grant.subject().revision_id()]).await.expect("masked number still authenticated"); }
                        "future" => { owner.client().execute(&format!("UPDATE access.{table} SET {at}=clock_timestamp()+interval '1 hour' WHERE id=$1"), &[&id]).await.expect("future state"); }
                        "older" => { owner.client().execute("DELETE FROM access.terminations WHERE id=$1", &[&old]).await.expect("broken end outside selected grant"); }
                        "query" => { owner.client().batch_execute(&format!("REVOKE SELECT ON access.{table} FROM {}", database.user)).await.expect("temporary query failure"); }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    // The record carries its own Grant identity even on an out-of-range page.
                    let query = if case == "parent page" && matches!(history, History::Resumptions) { json!({"grantId":grant.id(),"page":2,"size":1}) } else { selected() };
                    if matches!(case, "parent" | "parent page" | "older") {
                        let page = history.list(&services, &reader, query).await.expect("classification does not consume unrelated parent history");
                        assert_eq!(page.0, 1);
                        assert_eq!(page.1.len(), usize::from(!(case == "parent page" && matches!(history, History::Resumptions))));
                        services.ready().await.expect("unread damage does not close admission");
                        assert!(services.grant().verify(owner.client()).await.is_err(), "startup rejects {case}");
                        owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                        assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                        return;
                    }
                    let error = history.list(&services, &reader, query).await.expect_err("integrity failure cannot become a page");
                    assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                    assert_eq!(crate::runtime::work::Failure::unreliable(&error), case != "query", "{case}: {error:?}");
                    assert_eq!(services.ready().await.is_err(), case != "query", "{case}");
                    if case == "query" {
                        owner.client().batch_execute(&format!("GRANT SELECT ON access.{table} TO {}", database.user)).await.expect("restore test privilege");
                        assert_eq!(history.list(&services, &reader, selected()).await.expect("fresh page").0, 1);
                    }
                    owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded state history integrity scenario");
            }
        }));
    }

    #[test]
    fn suspension_list_rejects_damage_and_closes_admission() {
        state_history_integrity(History::Suspensions);
    }

    #[test]
    fn resumption_list_rejects_damage_and_closes_admission() {
        state_history_integrity(History::Resumptions);
    }

    fn state_history_snapshot(history: History) {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::StalledRoleCount, Fault::StalledRolePage] {
                timeout(Duration::from_secs(40), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = crate::access::role::tests::start_snapshot(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                    let (manager, reader, initial) = Box::pin(super::suspension::tests::participants(&database, &services, &mut supplier)).await;
                    Box::pin(history.cycle(&services, &manager, &reader)).await;
                    let role = Box::pin(services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"State snapshot reader","level":200,"permissions":[history.permission()]})).expect("role"))).await.expect("reader role");
                    Box::pin(assign_history_role(&services, &manager, Entry::Domain, "13900000000", crate::access::role::Kind::Custom)).await;
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    let concurrent = Box::pin(crate::correction::tests::register_person(&database, &services, &mut supplier, "13700000000", "Test Concurrent", "000000200002290029")).await;
                    let concurrent_grant = Box::pin(admit(&services, &manager, Entry::Domain, "13700000000", crate::access::role::Kind::Default)).await;
                    let expected = history.facts(observer.client(), initial.identity().person_id()).await;
                    proxy.arm(fault);
                    let reading = async { history.list(&services, &reader, json!({})).await };
                    let changing = async {
                        timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("list reached count or page after authority");
                        Box::pin(services.role().update_workforce(manager.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":"State access removed","level":200,"permissions":[]})).expect("remove permission"))).await.expect("concurrent permission loss");
                        if matches!(history, History::Terminations) {
                            Box::pin(services.grant().terminate(concurrent.clone(), operation(), concurrent_grant.id())).await.expect("concurrent real exit");
                        } else {
                            Box::pin(state_cycle(&services, &manager, "13700000000")).await;
                        }
                        proxy.release();
                    };
                    let (page, ()) = tokio::join!(reading, changing);
                    assert_eq!(page.expect("original complete snapshot"), (1, expected));
                    let latest = history.facts(observer.client(), initial.identity().person_id()).await;
                    assert_eq!(latest.len(), 2, "independent committed facts");
                    assert!(matches!(history.list(&services, &reader, json!({})).await, Err(Error::Denied)));
                    assert_eq!(history.list(&services, &manager, json!({})).await.expect("new snapshot"), (2, latest));
                    let target = services.current().read(&manager).await.expect("account target");
                    Box::pin(services.suspension().suspend(reader, operation(), target.id(), serde_json::from_value(json!({"personId":target.person_id(),"revisionId":target.revision_id(),"mobile":"13800000000"})).expect("account suspension"))).await.expect("account suspension revokes reader session");
                    assert_eq!(history.list(&services, &manager, json!({})).await.expect_err("suspended account denied").failure(), super::Failure::Invalid);
                    services.ready().await.expect("ordinary denial"); observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded state snapshot scenario");
            }
        }));
    }

    #[test]
    fn suspension_list_authority_count_and_page_share_a_snapshot() {
        state_history_snapshot(History::Suspensions);
    }

    #[test]
    fn resumption_list_authority_count_and_page_share_a_snapshot() {
        state_history_snapshot(History::Resumptions);
    }

    #[test]
    fn termination_list_authority_count_and_page_share_a_snapshot() {
        state_history_snapshot(History::Terminations);
    }

    fn state_history_finalization(history: History) {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(55), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                let (reader, target, _) = Box::pin(super::suspension::tests::participants(&database, &services, &mut supplier)).await;
                Box::pin(history.cycle(&services, &reader, &target)).await;
                let observer = database.connections("postgres").recover().await.expect("backend observer");
                let unrelated = database.connections(&database.user).recover().await.expect("unrelated transaction");
                unrelated.client().batch_execute("BEGIN; SELECT pg_advisory_xact_lock(2147483647,2147483647)").await.expect("unrelated lock");
                let unrelated_pid: i32 = unrelated.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("PID").get(0);
                for case in ["cancel", "count timeout", "page timeout", "commit", "finish"] {
                    proxy.arm(if case == "page timeout" { Fault::StalledRolePage } else { Fault::StalledRoleCount });
                    let service = services.grant().clone();
                    let actor = reader.clone();
                    let task = tokio::spawn(async move {
                        match history {
                            History::Suspensions => service.list_suspensions(&actor, filter()).await.map(|page| page.count()),
                            History::Resumptions => service.list_resumptions(&actor, filter()).await.map(|page| page.count()),
                            History::Terminations => service.list_terminations(&actor, filter()).await.map(|page| page.count()),
                        }
                    });
                    let pid = timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("exact list backend");
                    assert_ne!(pid, unrelated_pid);
                    if case == "cancel" {
                        task.abort();
                        assert!(task.await.is_err_and(|error| error.is_cancelled()));
                    } else {
                        match case {
                            "commit" => { proxy.arm(Fault::LostReadCommit); proxy.release(); }
                            "finish" => { proxy.arm(Fault::StalledReadCommit); proxy.release(); }
                            "count timeout" | "page timeout" => {}
                            _ => unreachable!(),
                        }
                        let error = timeout(Duration::from_secs(5), task).await.expect("bounded list").expect("read task").expect_err("unfinished page withheld");
                        assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                    }
                    proxy.release();
                        timeout(Duration::from_secs(3), async {
                            while observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("backend lifetime").get::<_,bool>(0) { tokio::task::yield_now().await; }
                        }).await.expect("original backend ends before cleanup");
                        assert_eq!(history.list(&services, &reader, json!({})).await.expect("fresh complete page").0, 1);
                        services.ready().await.expect("ordinary failure keeps admission open");
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND state='idle in transaction') AND EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND locktype='advisory' AND granted)", &[&unrelated_pid]).await.expect("unrelated work survives").get::<_,bool>(0));
                }
                unrelated.client().batch_execute("ROLLBACK").await.expect("unrelated end"); unrelated.terminate().await.expect("unrelated connection ended");
                services.close(); services.drain().await.expect("drained"); let _ = context.stop().await;
                observer.terminate().await.expect("observer ended"); proxy.stop().await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded state list finalization");
        }));
    }

    #[test]
    fn suspension_list_withholds_unconfirmed_pages_and_ends_its_backend() {
        state_history_finalization(History::Suspensions);
    }

    #[test]
    fn resumption_list_withholds_unconfirmed_pages_and_ends_its_backend() {
        state_history_finalization(History::Resumptions);
    }

    #[test]
    fn termination_list_withholds_unconfirmed_pages_and_ends_its_backend() {
        state_history_finalization(History::Terminations);
    }

    #[test]
    fn state_lists_use_stable_index_order() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (reader, _, candidate) = Box::pin(super::suspension::tests::participants(&database, &services, &mut supplier)).await;
                let first = Box::pin(state_cycle(&services, &reader, "13900000000")).await;
                Box::pin(state_cycle(&services, &reader, "13900000000")).await;
                let owner = database.connections("postgres").recover().await.expect("isolated ordering fixture");
                let now = context.sample().await.expect("trusted fixture time");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("time tie fixture");
                for (table, time, template) in [("suspensions","suspended_at",super::suspension::COMPLETIONS),("resumptions","resumed_at",super::resumption::COMPLETIONS)] {
                    let rows = owner.client().query(&format!("SELECT operation_id,{time} AS happened_at FROM access.{table} ORDER BY sequence"), &[]).await.expect("original fixture records");
                    for row in rows {
                        let operation = row.get("operation_id");
                        super::record::retime(owner.client(), services.app().keys(), operation, now, now).await.expect("resealed equal-time record");
                        crate::kernel::completion::Seed::age(owner.client(), services.app().keys(), template, operation, row.get::<_,chrono::DateTime<chrono::Utc>>("happened_at")-now).await.expect("paired completion time");
                    }
                }
                let state = super::state::retime(owner.client(), services.app(), candidate.grant_id(), None, now, now).await.expect("paired final head");
                owner.client().batch_execute("COMMIT").await.expect("time fixture committed");
                crate::history::Seed::anchor(services.app().anchors(), &state).expect("fixture state anchor");
                let ids = owner.client().query("SELECT id FROM access.suspensions ORDER BY id DESC", &[]).await.expect("independent UUID ordering");
                assert_eq!(ids.len(),2);
                for history in [History::Suspensions,History::Resumptions] {
                    for (index,row) in ids.iter().enumerate() {
                        let (count,page) = history.list(&services,&reader,json!({"page":index+1,"size":1})).await.expect("equal-time page");
                        assert_eq!(count,2); assert_eq!(page.len(),1); assert_eq!(page[0]["id"],json!(row.get::<_,Uuid>(0)));
                    }
                }
                // Synthetic rows below test only query plans, not valid histories or capacity.
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("plan fixture");
                owner.client().execute("INSERT INTO access.suspensions(id,grant_id,person_id,role_id,assignment_id,operator_id,revision_id,suspended_at,sequence,operation_id,epoch,seal,seal_key_id,seal_version) SELECT gen_random_uuid(),grant_id,person_id,role_id,assignment_id,operator_id,revision_id,suspended_at,sequence+series.value+100,gen_random_uuid(),epoch,seal,seal_key_id,seal_version FROM access.suspensions CROSS JOIN generate_series(1,20000) series(value) WHERE id=$1", &[&first]).await.expect("suspension ordering sample");
                owner.client().execute("INSERT INTO access.resumptions(id,grant_id,person_id,role_id,assignment_id,operator_id,revision_id,resumed_at,sequence,operation_id,epoch,seal,seal_key_id,seal_version) SELECT suspension.id,resumption.grant_id,resumption.person_id,resumption.role_id,resumption.assignment_id,resumption.operator_id,resumption.revision_id,resumption.resumed_at,suspension.sequence+1,gen_random_uuid(),resumption.epoch,resumption.seal,resumption.seal_key_id,resumption.seal_version FROM access.suspensions suspension CROSS JOIN access.resumptions resumption WHERE suspension.sequence>100 AND resumption.id=$1", &[&first]).await.expect("resumption ordering sample");
                owner.client().batch_execute("ANALYZE access.suspensions; ANALYZE access.resumptions").await.expect("planner statistics");
                for sql in [
                    "EXPLAIN SELECT suspension.id FROM access.suspensions suspension WHERE ($1::uuid IS NULL OR suspension.person_id=$1) AND ($2::uuid IS NULL OR suspension.grant_id=$2) AND ($3::uuid IS NULL OR suspension.operation_id=$3) ORDER BY suspension.suspended_at DESC,suspension.id DESC LIMIT $4 OFFSET $5",
                    "EXPLAIN SELECT resumption.id FROM access.resumptions resumption WHERE ($1::uuid IS NULL OR resumption.person_id=$1) AND ($2::uuid IS NULL OR resumption.grant_id=$2) AND ($3::uuid IS NULL OR resumption.operation_id=$3) ORDER BY resumption.resumed_at DESC,resumption.id DESC LIMIT $4 OFFSET $5",
                ] {
                    let plan=owner.client().query(sql,&[&Option::<Uuid>::None,&Option::<Uuid>::None,&Option::<Uuid>::None,&20_i64,&0_i64]).await.expect("actual selector plan").iter().map(|row|row.get::<_,String>(0)).collect::<Vec<_>>().join("\n");
                    assert!(plan.starts_with("Limit") && (plan.contains("Index Scan") || plan.contains("Index Only Scan")) && !plan.contains("Sort"),"{plan}");
                }
                owner.client().batch_execute("ROLLBACK").await.expect("discard synthetic plan rows");
                owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded state ordering checks");
        }));
    }

    pub(crate) async fn admit(
        services: &crate::startup::Services,
        actor: &crate::identity::current::Actor,
        entry: Entry,
        mobile: &str,
        kind: crate::access::role::Kind,
    ) -> super::Grant {
        let candidate = services
            .grant()
            .candidate(
                actor,
                entry,
                mobile.parse().expect("mobile"),
                crate::pagination::Pagination::new(1, 20).expect("page"),
            )
            .await
            .expect("admission candidate");
        let selected = candidate
            .roles()
            .iter()
            .find(|role| role.kind() == kind)
            .expect("selected role");
        let subject = candidate.identity();
        let body = json!({"entry":entry.as_ref(),"target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":mobile,"assignmentId":candidate.assignment_id(),"suspensionId":candidate.suspension_id(),"grant":candidate.grant()},"role":{"id":selected.id(),"changeId":selected.change_id()}});
        Box::pin(
            services
                .grant()
                .create(actor.clone(), operation(), command(&body)),
        )
        .await
        .expect("real admission")
        .0
    }

    #[test]
    fn management_read_preserves_all_ranks_ended_grants_and_first_assignment() {
        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let higher = Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                let role = Box::pin(services.role().create_workforce(member.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"Admission reader","level":200,"permissions":["workforce.admission"]})).expect("role"))).await.expect("reader role");
                let own = Box::pin(admit(&services, &member, Entry::Domain, "13900000000", Kind::Custom)).await;
                for id in [higher.id(), own.id()] {
                    let (grant, assignment) = services.grant().read(&institution, id).await.expect("lower reader sees every rank and self");
                    assert_eq!(grant.id(), id);
                    assert_eq!(grant.status(), Status::Active);
                    assert_eq!(assignment.grant_id(), id);
                    assert_eq!(assignment.operation_id(), grant.operation_id());
                    assert_eq!(assignment.sequence(), grant.sequence());
                    assert_eq!(assignment.assigned_at(), grant.granted_at());
                    assert_eq!(assignment.subject().revision_id(), grant.subject().revision_id());
                    assert_eq!(assignment.operator().revision_id(), grant.operator().revision_id());
                }
                Box::pin(services.role().update_workforce(member.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":"Renamed reader","level":201,"permissions":["workforce.admission","workforce.roles"]})).expect("new definition"))).await.expect("later role definition");
                let (_, assignment) = services.grant().read(&institution, own.id()).await.expect("original role after definition changed");
                assert_eq!(assignment.role().id(), role.id());
                assert_eq!(assignment.role().change_id(), role.change_id());
                assert_eq!(assignment.role().name(), role.name());
                assert_eq!(assignment.role().level(), role.level());
                assert_eq!(assignment.role().permissions(), role.permissions());
                let candidate = services.assignment().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("page")).await.expect("replacement candidate");
                let selected = candidate.roles().iter().find(|role| role.kind() == Kind::Default).expect("replacement role");
                let subject = candidate.identity();
                let replacement = Box::pin(services.assignment().assign(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":"13800000000","grantId":candidate.grant_id(),"assignmentId":candidate.assignment_id()},"role":{"id":selected.id(),"changeId":selected.change_id()}})).expect("replacement input"))).await.expect("later assignment");
                let (_, first) = services.grant().read(&institution, higher.id()).await.expect("first assignment survives replacement");
                assert_ne!(first.id(), replacement.id());
                assert_eq!(first.role().kind(), Kind::Full);
                assert_eq!(first.operation_id(), higher.operation_id());
                Box::pin(services.correction().correct(institution.clone(), operation(), serde_json::from_value(json!({"target":{"personId":higher.subject().person_id(),"revisionId":higher.subject().revision_id()},"name":"Changed Person","idNumber":"00000020000229007X"})).expect("correction"))).await.expect("later identity");
                Box::pin(services.grant().terminate(member.clone(), operation(), higher.id())).await.expect("real exit");
                let (ended, assignment) = services.grant().read(&institution, higher.id()).await.expect("ended higher Grant is readable");
                assert_eq!(ended.status(), Status::Terminated);
                assert_eq!(ended.subject().revision_id(), higher.subject().revision_id());
                assert_eq!(ended.subject().name(), "T*");
                assert_eq!(assignment.subject().revision_id(), higher.subject().revision_id());
                assert_eq!(assignment.subject().name(), "T*");
                assert_eq!(assignment.role().kind(), Kind::Full);
                assert!(matches!(services.grant().read(&institution, super::Id(Uuid::new_v4())).await, Err(Error::Absent)));
                services.ready().await.expect("absence preserves readiness");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded single Grant read");
        }));
    }

    #[test]
    fn management_read_requires_exact_current_admission_permission() {
        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let higher = Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                assert!(matches!(services.grant().read(&institution, higher.id()).await, Err(Error::Denied)), "institution-only manager is denied");
                let role = Box::pin(services.role().create_workforce(member.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"Other manager","level":200,"permissions":["workforce.roles","workforce.assignments"]})).expect("role"))).await.expect("other management powers");
                let own = Box::pin(admit(&services, &member, Entry::Domain, "13900000000", Kind::Custom)).await;
                for id in [higher.id(), own.id(), super::Id(Uuid::new_v4())] {
                    assert!(matches!(services.grant().read(&institution, id).await, Err(Error::Denied)), "permission precedes target lookup");
                }
                let role = Box::pin(services.role().update_workforce(member.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":"Admission reader","level":200,"permissions":["workforce.admission"]})).expect("permission"))).await.expect("admission enabled");
                services.grant().read(&institution, higher.id()).await.expect("current permission allows read");
                let current = services.current().read(&institution).await.expect("subject");
                let target = json!({"target":{"accountId":current.id(),"personId":current.person_id(),"revisionId":current.revision_id(),"mobile":"13900000000"}});
                let paused = Box::pin(services.grant().suspend(member.clone(), operation(), own.id(), serde_json::from_value(target.clone()).expect("pause"))).await.expect("domain pause");
                assert!(matches!(services.grant().read(&institution, higher.id()).await, Err(Error::Denied)));
                assert_eq!(services.grant().read(&member, own.id()).await.expect("paused target readable").0.status(), Status::Suspended);
                Box::pin(services.grant().resume(member.clone(), operation(), paused.id(), serde_json::from_value(target).expect("resume"))).await.expect("resumption");
                services.grant().read(&institution, higher.id()).await.expect("restored access");
                Box::pin(services.role().update_workforce(member.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":"Other manager","level":200,"permissions":["workforce.roles"]})).expect("lost permission"))).await.expect("admission removed");
                assert!(matches!(services.grant().read(&institution, higher.id()).await, Err(Error::Denied)));
                Box::pin(services.grant().terminate(institution.clone(), operation(), own.id())).await.expect("exit");
                assert!(matches!(services.grant().read(&institution, higher.id()).await, Err(Error::Denied)));
                Box::pin(services.revocation().revoke(member.clone(), operation(), crate::identity::revocation::Scope::Current)).await.expect("revoked session");
                assert_eq!(services.grant().read(&member, higher.id()).await.err().expect("invalid actor").failure(), super::Failure::Invalid);
                services.ready().await.expect("business denials preserve readiness");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded single Grant authority");
        }));
    }

    #[test]
    fn management_read_rejects_damaged_facts_instead_of_absence() {
        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["older", "first", "proof", "operator", "name", "future", "grant", "invitation"] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let reader = register(&database, &services, &mut supplier).await;
                    Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                    let old = Box::pin(admit(&services, &reader, Entry::Domain, "13900000000", Kind::Default)).await;
                    Box::pin(services.grant().terminate(institution.clone(), operation(), old.id())).await.expect("retained earlier Grant");
                    let grant = Box::pin(admit(&services, &reader, Entry::Domain, "13900000000", Kind::Default)).await;
                    services.grant().read(&reader, grant.id()).await.expect("valid history before damage");
                    let id = if case == "invitation" {
                        let roles = services.role().list_workforce(&reader, crate::pagination::Pagination::new(1, 100).expect("page")).await.expect("roles");
                        let role = roles.roles().iter().find(|role| role.kind() == Kind::Default).expect("invitation role");
                        let expires_at = chrono::SubsecRound::trunc_subsecs(chrono::Utc::now() + chrono::Duration::hours(1), 6);
                        Box::pin(services.grant().create_invitation(reader.clone(), operation(), serde_json::from_value(json!({"role":{"id":role.id(),"changeId":role.change_id()},"capacity":1,"expiresAt":expires_at})).expect("invitation input"))).await.expect("real invitation");
                        super::Id(Uuid::new_v4())
                    } else {
                        grant.id()
                    };
                    let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                    match case {
                        "older" => { owner.client().execute("DELETE FROM access.terminations WHERE id=$1", &[&old.id()]).await.expect("broken retained end"); }
                        "first" => { owner.client().execute("DELETE FROM access.assignments WHERE grant_id=$1", &[&grant.id()]).await.expect("missing first assignment"); }
                        "proof" => { owner.client().execute("UPDATE access.grants SET seal_key_id=(SELECT max(id)+1 FROM security.keys) WHERE id=$1", &[&grant.id()]).await.expect("missing proof key"); }
                        "operator" => { owner.client().execute("UPDATE access.grants SET operator_id=gen_random_uuid() WHERE id=$1", &[&grant.id()]).await.expect("missing fixed operator"); }
                        "name" => { owner.client().execute("UPDATE identity.revisions SET name=id_number WHERE id=$1", &[&grant.subject().revision_id()]).await.expect("wrong field binding"); }
                        "future" => { owner.client().execute("UPDATE access.grants SET granted_at=statement_timestamp()+interval '1 hour',changed_at=statement_timestamp()+interval '1 hour' WHERE id=$1", &[&grant.id()]).await.expect("future fact"); }
                        "grant" => { owner.client().execute("DELETE FROM access.grants WHERE id=$1", &[&grant.id()]).await.expect("orphaned retained references"); }
                        "invitation" => { owner.client().execute("UPDATE access.invitations SET grant_id=$1", &[&id]).await.expect("orphaned invitation reference"); }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    if case == "older" {
                        assert_eq!(services.grant().read(&reader, id).await.expect("unrelated old records do not invalidate this Grant").0.id(), grant.id());
                        services.ready().await.expect("unread damage does not close admission");
                        assert!(services.grant().verify(owner.client()).await.is_err(), "startup still rejects the missing old record");
                        owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                        supplier.stop().await;
                        return;
                    }
                    if case == "invitation" {
                        assert!(matches!(services.grant().read(&reader, id).await, Err(Error::Absent)), "an unanchored random ID does not scan unrelated invitation records");
                        services.ready().await.expect("unread invitation does not close admission");
                        assert!(super::invitation::verify(owner.client(), &services.grant().roles, &services.grant().invitations, &services.grant().revisions, context.sample().await.expect("trusted time")).await.is_err(), "startup rejects the orphaned invitation");
                        owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                        assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                        return;
                    }
                    let error = services.grant().read(&reader, id).await.err().expect("damage must not produce a result");
                    assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                    assert!(crate::runtime::work::Failure::unreliable(&error), "{case}: {error:?}");
                    assert!(services.ready().await.is_err(), "damage closes admission");
                    owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded single Grant integrity case");
            }
        }));
    }

    #[test]
    fn history_retains_all_ranks_and_fixed_identities_after_exit_and_cancellation() {
        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                for actor in [&institution, &member] {
                    assert!(matches!(services.grant().list(actor, filter()).await, Err(Error::Denied)));
                }
                let first = Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                Box::pin(services.role().create_workforce(member.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"Admission reader","level":200,"permissions":["workforce.admission"]})).expect("role"))).await.expect("exact history permission");
                let own = Box::pin(admit(&services, &member, Entry::Domain, "13900000000", Kind::Custom)).await;
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let facts = observer.client().query("SELECT admission.id, admission.granted_at, admission.sequence, admission.operation_id, subject.person_id, subject.id AS revision_id, operator.person_id AS operator_person, operator.id AS operator_id FROM access.grants admission JOIN identity.revisions subject ON subject.id=admission.revision_id JOIN identity.revisions operator ON operator.id=admission.operator_id ORDER BY admission.granted_at DESC, admission.id DESC", &[]).await.expect("independent permanent facts before reading");
                assert_eq!(facts.len(), 2);
                let list = services.grant().list(&institution, filter()).await.expect("lower reader sees self and higher member");
                assert_eq!(list.count(), 2);
                assert_eq!(list.grants().iter().map(super::Grant::id).collect::<Vec<_>>(), [own.id(), first.id()]);
                for (grant, row) in list.grants().iter().zip(&facts) {
                    assert_eq!(grant.id(), row.get("id"));
                    assert_eq!(grant.granted_at(), row.get::<_, chrono::DateTime<chrono::Utc>>("granted_at"));
                    assert_eq!(grant.sequence(), row.get::<_, i64>("sequence"));
                    assert_eq!(grant.operation_id(), row.get("operation_id"));
                    assert_eq!(grant.subject().person_id(), row.get("person_id"));
                    assert_eq!(grant.subject().revision_id(), row.get("revision_id"));
                    assert_eq!(grant.operator().person_id(), row.get("operator_person"));
                    assert_eq!(grant.operator().revision_id(), row.get("operator_id"));
                    assert_eq!(grant.status(), Status::Active);
                }
                for (query, count, ids) in [
                    (json!({"page":1,"size":1}), 2, vec![own.id()]),
                    (json!({"page":2,"size":1}), 2, vec![first.id()]),
                    (json!({"page":i64::MAX,"size":1}), 2, vec![]),
                    (json!({"size":100,"personId":first.subject().person_id()}), 1, vec![first.id()]),
                    (json!({"grantId":own.id()}), 1, vec![own.id()]),
                    (json!({"operationId":first.operation_id()}), 1, vec![first.id()]),
                    (json!({"personId":first.subject().person_id(),"grantId":first.id(),"operationId":first.operation_id()}), 1, vec![first.id()]),
                    (json!({"personId":own.subject().person_id(),"grantId":first.id()}), 0, vec![]),
                    (json!({"operationId":operation()}), 0, vec![]),
                ] {
                    let page = services.grant().list(&institution, serde_json::from_value(query).expect("filter")).await.expect("filtered page");
                    assert_eq!(page.count(), count);
                    assert_eq!(page.grants().iter().map(super::Grant::id).collect::<Vec<_>>(), ids);
                }
                Box::pin(services.correction().correct(institution.clone(), operation(), serde_json::from_value(json!({"target":{"personId":first.subject().person_id(),"revisionId":first.subject().revision_id()},"name":"Changed Person","idNumber":"00000020000229007X"})).expect("correction"))).await.expect("later identity");
                let revised = services.current().read(&member).await.expect("new identity");
                Box::pin(services.grant().terminate(member.clone(), operation(), first.id())).await.expect("real exit");
                let rejoined = Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                Box::pin(services.cancellation().cancel_account(institution.clone(), operation(), revised.id(), serde_json::from_value(json!({"personId":revised.person_id(),"revisionId":revised.revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("real cancellation after rejoining");
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.grants)=3 AND EXISTS(SELECT 1 FROM access.terminations WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$2) AND NOT EXISTS(SELECT 1 FROM access.grants WHERE person_id=$2 AND termination IS NULL)", &[&first.id(), &revised.person_id()]).await.expect("committed ends before history").get::<_, bool>(0));
                let page = services.grant().list(&institution, filter()).await.expect("retained history");
                assert_eq!(page.count(), 3);
                assert_eq!(page.grants().iter().map(super::Grant::id).collect::<Vec<_>>(), [rejoined.id(), own.id(), first.id()]);
                assert_eq!(page.grants()[0].status(), Status::Terminated);
                assert_eq!(page.grants()[0].subject().revision_id(), revised.revision_id());
                assert_eq!(page.grants()[0].subject().name(), "C*");
                assert_eq!(page.grants()[1].operator().revision_id(), first.subject().revision_id());
                assert_eq!(page.grants()[1].operator().name(), "T*");
                assert_eq!(page.grants()[2].subject().revision_id(), first.subject().revision_id());
                assert_eq!(page.grants()[2].subject().name(), "T*");
                assert_eq!(page.grants()[2].status(), Status::Terminated);
                let personal = services.grant().list(&institution, serde_json::from_value(json!({"personId":revised.person_id()})).expect("personal filter")).await.expect("all retained grants");
                assert_eq!(personal.grants().iter().map(super::Grant::id).collect::<Vec<_>>(), [rejoined.id(), first.id()]);
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.grants", &[]).await.expect("read cannot write facts").get::<_, i64>(0), 3);
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded permanent Grant history");
        }));
    }

    #[test]
    fn history_requires_exact_current_permission_and_unsuspended_membership() {
        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                let role = Box::pin(services.role().create_workforce(member.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"History reader","level":200,"permissions":["workforce.roles"]})).expect("role"))).await.expect("different domain permission");
                let grant = Box::pin(admit(&services, &member, Entry::Domain, "13900000000", Kind::Custom)).await;
                assert!(matches!(services.grant().list(&institution, filter()).await, Err(Error::Denied)), "neither institutional nor role maintenance permission grants history");
                Box::pin(services.role().update_workforce(member.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":"History reader","level":200,"permissions":["workforce.admission"]})).expect("exact permission"))).await.expect("permission granted");
                assert_eq!(services.grant().list(&institution, filter()).await.expect("current permission").count(), 2);
                let current = services.current().read(&institution).await.expect("subject confirmation");
                let target = json!({"target":{"accountId":current.id(),"personId":current.person_id(),"revisionId":current.revision_id(),"mobile":"13900000000"}});
                let paused = Box::pin(services.grant().suspend(member.clone(), operation(), grant.id(), serde_json::from_value(target.clone()).expect("pause"))).await.expect("domain pause");
                assert!(matches!(services.grant().list(&institution, filter()).await, Err(Error::Denied)));
                let page = services.grant().list(&member, filter()).await.expect("other reader sees paused Grant");
                assert_eq!(page.grants()[0].status(), Status::Suspended);
                Box::pin(services.grant().resume(member.clone(), operation(), paused.id(), serde_json::from_value(target).expect("resume"))).await.expect("domain resumption");
                assert_eq!(services.grant().list(&institution, filter()).await.expect("restored access").grants()[0].status(), Status::Active);
                Box::pin(services.grant().terminate(institution.clone(), operation(), grant.id())).await.expect("self exit");
                assert!(matches!(services.grant().list(&institution, filter()).await, Err(Error::Denied)));
                assert_eq!(services.grant().list(&member, filter()).await.expect("ended Grant remains visible").grants()[0].status(), Status::Terminated);
                Box::pin(services.revocation().revoke(member.clone(), operation(), crate::identity::revocation::Scope::Current)).await.expect("session revoked");
                assert_eq!(services.grant().list(&member, filter()).await.err().expect("old actor rejected").failure(), super::Failure::Invalid);
                services.ready().await.expect("business denials preserve readiness");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded history authority");
        }));
    }

    #[test]
    fn history_authority_count_and_states_share_one_snapshot() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::access::{
            appointment::tests::{mobile, register_numbered},
            role::Kind,
        };
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["count", "page", "retry count", "retry page", "finished"] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = crate::access::role::tests::start_snapshot(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let member = register(&database, &services, &mut supplier).await;
                    let first = Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                    let role = Box::pin(services.role().create_workforce(member.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"Snapshot reader","level":200,"permissions":["workforce.admission"]})).expect("reader role"))).await.expect("reader role");
                    let own = Box::pin(admit(&services, &member, Entry::Domain, "13900000000", Kind::Custom)).await;
                    let other = Box::pin(register_numbered(&database, &services, &mut supplier, 10)).await;
                    let fault = if case == "finished" { Fault::PausedReadEnd } else if case.ends_with("count") { Fault::StalledRoleCount } else { Fault::StalledRolePage };
                    proxy.arm(fault);
                    let service = services.grant().clone(); let actor = institution.clone();
                    let task = tokio::spawn(async move { service.list(&actor, filter()).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("exact snapshot boundary");
                    let changed = case.starts_with("retry") || case == "finished";
                    if changed {
                        Box::pin(services.role().update_workforce(member.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":"Permission removed","level":200,"permissions":[]})).expect("permission loss"))).await.expect("concurrent permission loss");
                        Box::pin(services.grant().terminate(member.clone(), operation(), first.id())).await.expect("concurrent old Grant ends");
                    }
                    let added = Box::pin(admit(&services, &institution, Entry::Institution, &mobile(10), Kind::Full)).await;
                    proxy.release();
                    let result = timeout(Duration::from_secs(5), task).await.expect("bounded snapshot result").expect("reader task");
                    if case.starts_with("retry") {
                        assert!(matches!(result, Err(Error::Denied)), "a changed anchor reruns identity and authority for the complete snapshot");
                    } else {
                        let page = result.unwrap_or_else(|error| panic!("{case}: {error:?}"));
                        assert_eq!(page.count(), 2);
                        assert_eq!(page.grants().iter().map(super::Grant::id).collect::<Vec<_>>(), [own.id(), first.id()]);
                        assert!(page.grants().iter().all(|grant| grant.status() == Status::Active));
                    }
                    if changed {
                        assert!(matches!(services.grant().list(&institution, filter()).await, Err(Error::Denied)), "fresh reads see lost permission");
                    } else {
                        assert_eq!(services.grant().list(&institution, filter()).await.expect("fresh authorized page").count(), 3);
                    }
                    let latest = services.grant().list(&other, filter()).await.expect("independent current page");
                    assert_eq!(latest.count(), 3);
                    assert_eq!(latest.grants()[0].id(), added.id());
                    assert_eq!(latest.grants().iter().find(|grant| grant.id() == first.id()).expect("original Grant").status(), if changed { Status::Terminated } else { Status::Active });
                    services.ready().await.expect("business denial preserves readiness");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("{case}: {error}"));
            }
        }));
    }

    #[test]
    fn history_rejects_future_cancellation_for_current_and_earlier_grants() {
        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for earlier in [false, true] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let member = register(&database, &services, &mut supplier).await;
                    let first = Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                    Box::pin(services.role().create_workforce(member.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"Cancellation reader","level":200,"permissions":["workforce.admission"]})).expect("reader"))).await.expect("reader role");
                    Box::pin(admit(&services, &member, Entry::Domain, "13900000000", Kind::Custom)).await;
                    if earlier {
                        Box::pin(services.grant().terminate(member.clone(), operation(), first.id())).await.expect("real earlier exit");
                        Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                    }
                    let current = services.current().read(&member).await.expect("target confirmation");
                    Box::pin(services.cancellation().cancel_account(institution.clone(), operation(), current.id(), serde_json::from_value(json!({"personId":current.person_id(),"revisionId":current.revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("real cancellation");
                    let selected = || super::Filter { grant_id: Some(first.id()), ..filter() };
                    assert_eq!(services.grant().list(&institution, selected()).await.expect("valid retained end").grants()[0].status(), Status::Terminated);
                    let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated timestamp damage");
                    assert_eq!(owner.client().execute("UPDATE identity.cancellations SET cancelled_at=clock_timestamp()+interval '1 hour' WHERE person_id=$1", &[&current.person_id()]).await.expect("future cancellation"), 1);
                    owner.client().batch_execute("COMMIT").await.expect("fault committed");
                    assert!(owner.client().query_one("SELECT cancelled_at>clock_timestamp() FROM identity.cancellations WHERE person_id=$1", &[&current.person_id()]).await.expect("independent future-time evidence").get::<_, bool>(0));
                    assert_eq!(services.grant().list(&institution, selected()).await.expect("sealed current state does not derive its end from cancellation history").grants()[0].status(), Status::Terminated);
                    services.ready().await.expect("unread cancellation does not close admission");
                    assert!(services.grant().verify(owner.client()).await.is_err(), "startup still authenticates the cancellation source");
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.grants", &[]).await.expect("read leaves permanent facts untouched").get::<_, i64>(0), if earlier { 3 } else { 2 });
                    owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded cancellation time validation");
            }
        }));
    }

    #[test]
    fn history_batches_full_pages_without_per_person_round_trips() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::{
            access::{
                appointment::tests::{mobile, register_numbered},
                role::Kind,
            },
            identity::authentication,
        };

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let proxy = Proxy::start(&server).await;
            let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
            let institution = actor(&database, &services, &mut supplier).await;
            let reader = register(&database, &services, &mut supplier).await;
            Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
            for index in 10..109 {
                timeout(Duration::from_secs(60), Box::pin(async {
                    Box::pin(register_numbered(&database, &services, &mut supplier, index)).await;
                    Box::pin(admit(&services, &reader, Entry::Domain, &mobile(index), Kind::Default)).await;
                })).await.expect("bounded account and admission preparation");
            }
            timeout(Duration::from_secs(60), Box::pin(async {
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let rows = observer.client().query("SELECT id, person_id, revision_id, operator_id, operation_id FROM access.grants ORDER BY granted_at DESC, id DESC", &[]).await.expect("committed full page before reading");
                assert_eq!(rows.len(), 100);
                assert_eq!(observer.client().query_one("SELECT count(DISTINCT person_id) FROM access.grants", &[]).await.expect("distinct actual people").get::<_, i64>(0), 100);
                let mut baseline = None;
                for size in [1, 100] {
                    proxy.arm(Fault::DelayedStateQueries);
                    let started = std::time::Instant::now();
                    let result = services.grant().list(&reader, serde_json::from_value(json!({"size":size})).expect("page filter")).await;
                    let queries = proxy.delayed_queries();
                    eprintln!("Grant history size={size}: {:?}; queries delayed by 8 ms: {queries}", started.elapsed());
                    let page = result.expect("complete history within the existing read deadline");
                    assert!(queries > 0, "the delayed history transaction was observed");
                    assert_eq!(*baseline.get_or_insert(queries), queries, "a full page uses the same queries as a single row");
                    assert_eq!(page.count(), 100);
                    assert_eq!(page.grants().len(), size);
                    for (grant, row) in page.grants().iter().zip(&rows) {
                        assert_eq!(grant.id(), row.get("id"));
                        assert_eq!(grant.operation_id(), row.get("operation_id"));
                        assert_eq!(grant.subject().person_id(), row.get("person_id"));
                        assert_eq!(grant.subject().revision_id(), row.get("revision_id"));
                        assert_eq!(grant.operator().revision_id(), row.get("operator_id"));
                        assert_eq!(grant.status(), Status::Active);
                        assert!(matches!(grant.subject().name(), "H*" | "T*"));
                    }
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.grants", &[]).await.expect("reading preserves facts").get::<_, i64>(0), 100);
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded full history acceptance");
        }));
    }

    #[test]
    fn history_first_page_uses_index_order_for_retained_grants() {
        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let grant = Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                assert_eq!(services.grant().list(&member, filter()).await.expect("real baseline read").count(), 1);
                let owner = database.connections("postgres").recover().await.expect("isolated plan material");
                // Synthetic rows exercise only the selector's plan, not Grant decoding or capacity.
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated plan preparation");
                owner.client().execute("INSERT INTO access.grants (id,person_id,domain,entry,operator_id,revision_id,granted_at,sequence,version,operation_id,role_id,assignment_id,termination,epoch,changed_at,seal,seal_key_id,seal_version) SELECT gen_random_uuid(),person_id,domain,entry,operator_id,revision_id,granted_at,sequence+series.value,version+series.value,gen_random_uuid(),role_id,assignment_id,'self',epoch,changed_at,seal,seal_key_id,seal_version FROM access.grants CROSS JOIN generate_series(1,20000) series(value) WHERE id=$1", &[&grant.id()]).await.expect("retained ordering sample");
                owner.client().batch_execute("ANALYZE access.grants").await.expect("real planner statistics");
                let plan = owner.client().query("EXPLAIN SELECT id FROM access.grants WHERE ($1::uuid IS NULL OR person_id=$1) AND ($2::uuid IS NULL OR id=$2) AND ($3::uuid IS NULL OR operation_id=$3) ORDER BY granted_at DESC,id DESC LIMIT $4 OFFSET $5", &[&Option::<crate::identity::person::Id>::None, &Option::<super::Id>::None, &Option::<operation::Id>::None, &20_i64, &0_i64]).await.expect("actual parameterized page selector").iter().map(|row| row.get::<_, String>(0)).collect::<Vec<_>>().join("\n");
                assert!(plan.starts_with("Limit"), "{plan}");
                assert!((plan.contains("Index Scan") || plan.contains("Index Only Scan")) && !plan.contains("Sort"), "first page must not sort all retained rows: {plan}");
                owner.client().batch_execute("ROLLBACK").await.expect("discard synthetic plan rows");
                owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded ordering plan check");
        }));
    }

    #[test]
    fn history_breaks_equal_time_ties_by_id_across_pages() {
        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(25),
                Box::pin(async {
                    let database = Database::initialized(server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let reader = register(&database, &services, &mut supplier).await;
                    let first = Box::pin(admit(
                        &services,
                        &institution,
                        Entry::Institution,
                        "13800000000",
                        Kind::Full,
                    ))
                    .await;
                    let second = Box::pin(admit(
                        &services,
                        &reader,
                        Entry::Domain,
                        "13900000000",
                        Kind::Default,
                    ))
                    .await;
                    let owner = database
                        .connections("postgres")
                        .recover()
                        .await
                        .expect("isolated ordering fixture");
                    // Reseal the paired state, first assignment and completion at the same time.
                    owner
                        .client()
                        .batch_execute("BEGIN; SET LOCAL session_replication_role=replica")
                        .await
                        .expect("synthetic time tie");
                    let now = context.sample().await.expect("fixture time");
                    super::record::retime(
                        owner.client(),
                        services.app().keys(),
                        first.operation_id(),
                        second.granted_at(),
                        now,
                    )
                    .await
                    .expect("sealed first assignment");
                    let state = super::state::retime(
                        owner.client(),
                        services.app(),
                        first.id(),
                        Some(second.granted_at()),
                        second.granted_at(),
                        now,
                    )
                    .await
                    .expect("sealed equal-time Grant");
                    crate::kernel::completion::Seed::age(
                        owner.client(),
                        services.app().keys(),
                        super::command::COMPLETIONS,
                        first.operation_id(),
                        first.granted_at() - second.granted_at(),
                    )
                    .await
                    .expect("paired admission completion");
                    owner
                        .client()
                        .batch_execute("COMMIT")
                        .await
                        .expect("fixture committed");
                    crate::history::Seed::anchor(services.app().anchors(), &state)
                        .expect("fixture state anchor");
                    let rows = owner
                        .client()
                        .query(
                            "SELECT id,granted_at FROM access.grants ORDER BY id DESC",
                            &[],
                        )
                        .await
                        .expect("independent UUID order");
                    assert_eq!(rows.len(), 2);
                    assert_eq!(
                        rows[0].get::<_, chrono::DateTime<chrono::Utc>>(1),
                        rows[1].get::<_, chrono::DateTime<chrono::Utc>>(1)
                    );
                    for (index, row) in rows.iter().enumerate() {
                        let page = services
                            .grant()
                            .list(
                                &reader,
                                serde_json::from_value(json!({"page":index+1,"size":1}))
                                    .expect("single-row page"),
                            )
                            .await
                            .expect("stable equal-time page");
                        assert_eq!(page.count(), 2);
                        assert_eq!(page.grants().len(), 1);
                        assert_eq!(page.grants()[0].id(), row.get("id"));
                    }
                    owner.terminate().await.expect("owner ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    assert!(
                        timeout(Duration::from_millis(30), supplier.request())
                            .await
                            .is_err()
                    );
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded equal-time pagination");
        }));
    }

    #[test]
    fn history_rejects_damaged_facts_and_recovers_query_failures() {
        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["first", "proof", "subject", "operator", "name", "id_number", "future", "older", "account_missing", "account_location", "account_seal", "query"] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let reader = register(&database, &services, &mut supplier).await;
                    Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                    let old = Box::pin(admit(&services, &reader, Entry::Domain, "13900000000", Kind::Default)).await;
                    Box::pin(services.grant().terminate(institution.clone(), operation(), old.id())).await.expect("retained earlier history");
                    let grant = Box::pin(admit(&services, &reader, Entry::Domain, "13900000000", Kind::Default)).await;
                    let selected = || super::Filter { grant_id: Some(grant.id()), ..filter() };
                    assert_eq!(services.grant().list(&reader, selected()).await.expect("valid selected history").count(), 1);
                    let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated post-startup damage");
                    match case {
                        "first" => { owner.client().execute("DELETE FROM access.assignments WHERE grant_id=$1", &[&grant.id()]).await.expect("missing first assignment"); }
                        "proof" => { owner.client().execute("UPDATE access.grants SET seal_key_id=(SELECT max(id)+1 FROM security.keys) WHERE id=$1", &[&grant.id()]).await.expect("missing proof key"); }
                        "subject" => { owner.client().execute("UPDATE access.grants SET revision_id=gen_random_uuid() WHERE id=$1", &[&grant.id()]).await.expect("missing fixed subject"); }
                        "operator" => { owner.client().execute("UPDATE access.grants SET operator_id=gen_random_uuid() WHERE id=$1", &[&grant.id()]).await.expect("missing fixed operator"); }
                        "name" => { owner.client().execute("UPDATE identity.revisions SET name=id_number WHERE id=$1", &[&grant.subject().revision_id()]).await.expect("wrong envelope field binding"); }
                        "id_number" => { owner.client().execute("UPDATE identity.revisions SET id_number=set_byte(id_number,octet_length(id_number)-1,get_byte(id_number,octet_length(id_number)-1)#1) WHERE id=$1", &[&grant.subject().revision_id()]).await.expect("masked identity still authenticates number"); }
                        "future" => { owner.client().execute("UPDATE access.grants SET granted_at=statement_timestamp()+interval '1 hour',changed_at=statement_timestamp()+interval '1 hour' WHERE id=$1", &[&grant.id()]).await.expect("future retained fact"); }
                        "older" => { owner.client().execute("DELETE FROM access.terminations WHERE id=$1", &[&old.id()]).await.expect("damaged end outside selected page"); }
                        "account_missing" => { owner.client().execute("DELETE FROM identity.accounts WHERE person_id=$1", &[&grant.subject().person_id()]).await.expect("remove an anchored account from the batch"); }
                        "account_location" => { owner.client().execute("UPDATE identity.accounts SET person_id=gen_random_uuid() WHERE person_id=$1", &[&grant.subject().person_id()]).await.expect("move an anchored account outside the batch selection"); }
                        "account_seal" => { owner.client().execute("UPDATE identity.accounts SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE person_id=$1", &[&grant.subject().person_id()]).await.expect("damage a selected account seal"); }
                        "query" => { owner.client().batch_execute(&format!("REVOKE SELECT ON access.grants FROM {}", database.user)).await.expect("ordinary query failure"); }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("fault committed");
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.grants", &[]).await.expect("permanent count before read").get::<_, i64>(0), 3);
                    if matches!(case, "first" | "older") {
                        assert_eq!(services.grant().list(&reader, selected()).await.expect("list does not consume unrelated records").count(), 1);
                        services.ready().await.expect("unread records do not close admission");
                        assert!(services.grant().verify(owner.client()).await.is_err(), "startup rejects missing history");
                        owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                        assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                        return;
                    }
                    let error = services.grant().list(&reader, selected()).await.err().expect("no partial history");
                    assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                    assert_eq!(crate::runtime::work::Failure::unreliable(&error), case != "query", "{case}: {error:?}");
                    assert_eq!(services.ready().await.is_err(), case != "query");
                    for secret in ["Test Person", "000000200002290010", "13900000000"] { assert!(!format!("{error:?}").contains(secret)); }
                    if case == "query" {
                        owner.client().batch_execute(&format!("GRANT SELECT ON access.grants TO {}", database.user)).await.expect("restore test permission");
                        assert_eq!(services.grant().list(&reader, selected()).await.expect("fresh complete page after recovery").count(), 1);
                    }
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.grants", &[]).await.expect("no repair by reading").get::<_, i64>(0), 3);
                    owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded history integrity case");
            }
        }));
    }

    #[test]
    fn history_withholds_unfinished_pages_and_ends_only_its_own_backend() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::access::role::Kind;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let reader = register(&database, &services, &mut supplier).await;
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                let observer = database.connections("postgres").recover().await.expect("independent backend observer");
                let unrelated = database.connections(&database.user).recover().await.expect("unrelated connection");
                unrelated.client().batch_execute("BEGIN; SELECT pg_catalog.pg_advisory_xact_lock(2147483647, 2147483647)").await.expect("unrelated transaction and lock");
                let unrelated_pid: i32 = unrelated.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("unrelated PID").get(0);
                for case in ["cancel", "count timeout", "page timeout", "commit", "finish"] {
                    proxy.arm(if case == "page timeout" { Fault::StalledRolePage } else { Fault::StalledRoleCount });
                    let service = services.grant().clone(); let actor = reader.clone();
                    let task = tokio::spawn(async move { service.list(&actor, filter()).await });
                    let pid = timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("exact list backend");
                    assert_ne!(pid, unrelated_pid);
                    if case == "cancel" { task.abort(); assert!(task.await.is_err_and(|error| error.is_cancelled())); }
                    else {
                        match case {
                            "commit" => { proxy.arm(Fault::LostReadCommit); proxy.release(); }
                            "finish" => { proxy.arm(Fault::StalledReadCommit); proxy.release(); }
                            "count timeout" | "page timeout" => {}
                            _ => unreachable!(),
                        }
                        let error = timeout(Duration::from_secs(5), task).await.expect("bounded history read").expect("read task").err().expect("unfinished page withheld");
                        assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                    }
                    proxy.release();
                        timeout(Duration::from_secs(3), async {
                            while observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("actual backend lifetime").get::<_, bool>(0) { tokio::task::yield_now().await; }
                        }).await.expect("original backend ends before fixture cleanup");
                        services.ready().await.expect("ordinary read failure preserves readiness");
                        assert_eq!(services.grant().list(&reader, filter()).await.expect("fresh complete page").count(), 1);
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND state='idle in transaction') AND EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND locktype='advisory' AND granted)", &[&unrelated_pid]).await.expect("unrelated work survives cleanup").get::<_, bool>(0));
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.grants", &[]).await.expect("read cannot mutate business facts").get::<_, i64>(0), 1);
                }
                unrelated.client().batch_execute("ROLLBACK").await.expect("unrelated transaction ended"); unrelated.terminate().await.expect("unrelated connection ended");
                services.close(); services.drain().await.expect("drained"); let _ = context.stop().await;
                timeout(Duration::from_secs(3), async {
                    while observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND usename=$1)", &[&database.user]).await.expect("remaining runtime backends").get::<_, bool>(0) { tokio::task::yield_now().await; }
                }).await.expect("all runtime backends ended before fixture cleanup");
                observer.terminate().await.expect("observer ended"); proxy.stop().await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded read finalization");
        }));
    }

    #[test]
    fn original_admissions_reject_missing_operator_revisions() {
        use fixture::database_proxy::Proxy;

        use crate::{identity::authentication, runtime::work::Failure};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            let mut failures = Vec::new();
            for entry in [Entry::Institution, Entry::Domain] {
                for recovering in [false, true] {
                    timeout(Duration::from_secs(35), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                        let institution = actor(&database, &services, &mut supplier).await;
                        let holder = register(&database, &services, &mut supplier).await;
                        let (manager, mobile) = if entry == Entry::Domain {
                            let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("domain manager candidate");
                            Box::pin(services.grant().create(institution, operation(), command(&input(&candidate, 1)))).await.expect("actual domain authority");
                            (holder, "13900000000")
                        } else { (institution, "13800000000") };
                        let candidate = services.grant().candidate(&manager, entry, mobile.parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("qualified admission candidate");
                        let mut body = input(&candidate, 0);
                        body["entry"] = json!(entry.as_ref());
                        body["target"]["mobile"] = json!(mobile);
                        let operation_id = operation();
                        let result = if recovering {
                            Box::pin(crate::recovery::after_revision_loss(&database, &proxy, &manager,
                                services.grant().create(manager.clone(), operation_id, command(&body)))).await
                        } else {
                            let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation_id, command(&body))).await.expect("actual committed admission");
                            let owner = database.connections("postgres").recover().await.expect("isolated damage owner");
                            owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated missing reference");
                            assert_eq!(owner.client().execute("DELETE FROM identity.revisions WHERE id=$1", &[&grant.operator().revision_id()]).await.expect("remove the exact operator revision"), 1);
                            owner.client().batch_execute("COMMIT").await.expect("damage committed");
                            owner.terminate().await.expect("owner ended");
                            Box::pin(services.grant().create(manager.clone(), operation_id, command(&body))).await
                        };
                        let classified = match &result {
                            Err(Error::Command(crate::kernel::Error::Unknown(fault))) if recovering => std::iter::once((fault.stage(),fault.kind())).chain(fault.later()).any(|(_,kind)| matches!(kind,crate::kernel::FaultKind::Revision(error) if error.unreliable()) || matches!(kind,crate::kernel::FaultKind::Access(Error::Assignment(crate::access::assignment::ReadError::Revision(crate::identity::person::revision::ReadError::Missing))))),
                            Err(Error::Command(crate::kernel::Error::Corrupt(fault))) if !recovering => matches!(fault.kind(), crate::kernel::FaultKind::Identity(error) if error.unreliable()),
                            Err(Error::Unknown { recovery, .. }) if recovering => matches!(recovery.as_ref(), Error::Decode(_)) && recovery.unreliable(),
                            Err(error @ Error::Role(crate::access::role::Error::Current(crate::identity::current::Error::Facts { rule: "the current revision belongs to the account's person" }))) if !recovering && entry==Entry::Institution => error.unreliable(),
                            Err(error @ Error::Current(crate::identity::current::Error::Facts { rule: "the current revision belongs to the account's person" })) if !recovering && entry==Entry::Domain => error.unreliable(),
                            _ => false,
                        };
                        let closed = services.ready().await.is_err();
                        if !classified || !closed {
                            failures.push(format!("{entry:?}, recovering={recovering}, closed={closed}: {:?}", result.as_ref().err()));
                        }
                        if closed {
                            assert!(matches!(Box::pin(services.grant().create(manager, operation_id, command(&body))).await,
                                Err(Error::Command(crate::kernel::Error::Unavailable(fault))) if fault.stage()==crate::kernel::Stage::Admission && matches!(fault.kind(), crate::kernel::FaultKind::Closed)), "later work is not accepted");
                        }
                        services.close(); services.drain().await.expect("drained");
                        let observer = database.connections(&database.user).recover().await.expect("independent durable facts");
                        let row = observer.client().query_one("SELECT admission.sequence, person.sequence AS person_sequence, (SELECT count(*) FROM access.assignments WHERE grant_id=admission.id) AS assignments, (SELECT count(*) FROM access.grants WHERE id=admission.id AND termination IS NULL) AS members, NOT EXISTS(SELECT 1 FROM identity.revisions WHERE id=admission.operator_id) AS missing FROM access.grants admission JOIN identity.persons person ON person.id=admission.person_id WHERE admission.operation_id=$1", &[&operation_id]).await.expect("retained original is not an absent operation");
                        assert_eq!((row.get::<_, i64>("sequence"), row.get::<_, i64>("person_sequence")), (1, 1));
                        assert_eq!((row.get::<_, i64>("assignments"), row.get::<_, i64>("members")), (1, 1));
                        assert!(row.get::<_, bool>("missing"));
                        observer.terminate().await.expect("observer ended");
                        context.stop().await.expect("stopped"); proxy.stop().await;
                        assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                    })).await.expect("damaged original admission deadline");
                }
            }
            assert!(failures.is_empty(), "damaged originals were not contained: {failures:?}");
        }));
    }

    #[test]
    fn accepted_admissions_recover_after_session_revocation() {
        use fixture::database_proxy::Proxy;

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            let mut failures = Vec::new();
            for entry in [Entry::Institution, Entry::Domain] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let holder = register(&database, &services, &mut supplier).await;
                    let (manager, mobile) = if entry == Entry::Domain {
                        let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("domain manager candidate");
                        Box::pin(services.grant().create(institution, operation(), command(&input(&candidate, 1)))).await.expect("actual domain authority");
                        (holder, "13900000000")
                    } else { (institution, "13800000000") };
                    let candidate = services.grant().candidate(&manager, entry, mobile.parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("qualified admission candidate");
                    let mut body = input(&candidate, 0);
                    body["entry"] = json!(if entry == Entry::Domain { "domain" } else { "institution" });
                    body["target"]["mobile"] = json!(mobile);
                    let operation_id = operation();
                    let result = Box::pin(crate::recovery::after_revocation(&database, &proxy, services.revocation(), &manager, services.grant().create(manager.clone(), operation_id, command(&body)))).await;
                    let observer = database.connections(&database.user).recover().await.expect("independent facts");
                    let row = observer.client().query_one("SELECT admission.id,admission.entry,admission.sequence,person.sequence AS person_sequence,assignment.id AS assignment FROM access.grants admission JOIN identity.persons person ON person.id=admission.person_id JOIN access.assignments assignment ON assignment.grant_id=admission.id AND assignment.id=admission.assignment_id WHERE admission.termination IS NULL AND admission.operation_id=$1 AND admission.person_id=$2", &[&operation_id,&candidate.identity().person_id()]).await.expect("one complete original admission");
                    assert_eq!(row.get::<_,String>("entry"), if entry == Entry::Domain { "domain" } else { "institution" });
                    assert_eq!((row.get::<_,i64>("sequence"),row.get::<_,i64>("person_sequence")), (1,1));
                    match result {
                        Ok((grant, assignment)) => { assert_eq!(grant.id(),row.get("id")); assert_eq!(assignment.id(),row.get("assignment")); }
                        Err(error) => failures.push(format!("{entry:?}: {error:?}")),
                    }
                    let Err(error) = Box::pin(services.grant().create(manager, operation_id, command(&body))).await else { panic!("new request must be refused"); };
                    assert_eq!(error.failure(), super::Failure::Invalid);
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.grants WHERE operation_id=$1", &[&operation_id]).await.expect("no replay writes").get::<_,i64>(0), 1);
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    services.ready().await.expect("paired facts survive restart");
                    services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped");
                    observer.terminate().await.expect("observer ended");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("admission recovery after session loss deadline");
            }
            assert!(failures.is_empty(), "accepted admissions lost their results: {failures:?}");
        }));
    }

    #[test]
    fn domain_admission_uses_only_its_selected_authority_and_preserves_the_original() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = register(&database, &services, &mut supplier).await;
                assert!(matches!(services.grant().candidate(&institution, Entry::Domain, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await, Err(Error::Denied)));
                let admission = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("institutional manager candidate");
                let (source, _) = Box::pin(services.grant().create(institution.clone(), operation(), command(&input(&admission, 1)))).await.expect("real full Workforce membership");
                assert!(matches!(services.grant().candidate(&manager, Entry::Institution, "13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await, Err(Error::Role(crate::access::role::Error::Denied))));
                assert!(matches!(services.grant().candidate(&manager, Entry::Domain, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await, Err(Error::Absent)));
                let candidate = services.grant().candidate(&manager, Entry::Domain, "13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("domain manager may admit a higher institutional account");
                assert_eq!(candidate.roles().len(), 1);
                assert!(candidate.roles()[0].permissions().is_empty());
                let mut body = input(&candidate, 0);
                body["entry"] = json!("domain");
                body["target"]["mobile"] = json!("13900000000");
                let mut elevated = body.clone();
                elevated["role"] = json!({"id":admission.roles()[0].id(),"changeId":admission.roles()[0].change_id()});
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), operation(), command(&elevated))).await, Err(Error::Denied)));
                elevated["role"]["changeId"] = json!(Uuid::new_v4());
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), operation(), command(&elevated))).await, Err(Error::RoleChanged)));
                let operation = operation();
                let (grant, assignment) = Box::pin(services.grant().create(manager.clone(), operation, command(&body))).await.expect("domain admission to the default role");
                let observer = database.connections(&database.user).recover().await.expect("independent runtime observer");
                let row = observer.client().query_one("SELECT admission.entry,person.sequence AS person_sequence,admission.sequence,assignment.sequence,admission.assignment_id,role.level,role.scope,admission.operator_id FROM access.grants admission JOIN identity.persons person ON person.id=admission.person_id JOIN access.assignments assignment ON assignment.grant_id=admission.id AND assignment.id=admission.assignment_id JOIN access.roles role ON role.id=admission.role_id WHERE admission.termination IS NULL AND admission.operation_id=$1", &[&operation]).await.expect("independent domain result");
                assert_eq!(row.get::<_, String>(0), "domain");
                assert_eq!((row.get::<_, i64>(1), row.get::<_, i64>(2), row.get::<_, i64>(3)), (1, 1, 1));
                assert_eq!(row.get::<_, crate::access::assignment::Id>(4), assignment.id());
                assert_eq!(row.get::<_, i16>(5), 1);
                assert_eq!(row.get::<_, String>(6), "workforce");
                assert_eq!(row.get::<_, crate::identity::person::revision::Id>(7), source.subject().revision_id());
                assert!(matches!(services.grant().candidate(&institution, Entry::Domain, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await, Err(Error::Denied)));
                let fresh = services.grant().candidate(&manager, Entry::Domain, "13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("lower active member is visible");
                let mut fresh_body = input(&fresh, 0);
                fresh_body["entry"] = json!("domain");
                fresh_body["target"]["mobile"] = json!("13900000000");
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), self::operation(), command(&fresh_body))).await, Err(Error::AlreadyAuthorized)));
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), self::operation(), command(&body))).await, Err(Error::TargetChanged)));
                let role = services.role().create(institution.clone(), self::operation(), serde_json::from_value(json!({"name":"Workforce Office","level":500,"permissions":["institution.workforce"]})).expect("institution role input")).await.expect("second real authority");
                let appointment = services.appointment().candidate(&institution, "13800000000".parse().expect("mobile")).await.expect("institutional appointment candidate");
                Box::pin(services.appointment().assign(institution.clone(), self::operation(), crate::access::appointment::tests::command(&appointment, &role))).await.expect("domain manager also receives explicit institutional authority");
                let mut switched = body.clone();
                switched["entry"] = json!("institution");
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), operation, command(&switched))).await, Err(Error::Mismatch)));
                let (replayed, repeated) = Box::pin(services.grant().create(manager.clone(), operation, command(&body))).await.expect("original domain result despite the unrelated institutional change");
                assert_eq!((replayed.id(), repeated.id()), (grant.id(), assignment.id()));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (replayed, repeated) = Box::pin(services.grant().create(manager.clone(), operation, command(&body))).await.expect("exact result after restart");
                assert_eq!((replayed.id(), repeated.id()), (grant.id(), assignment.id()));
                let target = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("creator cancellation candidate");
                Box::pin(services.cancellation().cancel_account(institution.clone(), self::operation(), target.identity().id(), serde_json::from_value(json!({"personId":target.identity().person_id(),"revisionId":target.identity().revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("real cancellation of the granting manager");
                assert!(matches!(Box::pin(services.grant().create(manager, operation, command(&body))).await, Err(Error::Current(crate::identity::current::Error::Invalid))));
                let current = services.grant().read_latest(&institution).await.expect("creator loss does not revoke the admitted member");
                assert_eq!(current.id(), grant.id());
                assert_eq!(current.status(), Status::Active);
                assert_eq!(current.operator().revision_id(), source.subject().revision_id());
                let observer = database.connections(&database.user).recover().await.expect("final independent observer");
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.grants)=2 AND (SELECT count(*) FROM access.assignments)=2 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=1 AND EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND termination IS NULL) AND EXISTS(SELECT 1 FROM access.grants WHERE id=$2 AND termination='cancellation')", &[&grant.id(), &source.id()]).await.expect("only creator membership ended; no replay writes").get::<_, bool>(0));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
            })).await.expect("domain admission lifecycle deadline");
        }));
    }

    #[test]
    fn domain_admission_hides_other_managers_at_the_same_level() {
        use crate::identity::{
            authentication::session::{
                Receipt,
                tests::{CODE, input as authentication, message},
            },
            current,
        };

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = register(&database, &services, &mut supplier).await;
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("first domain manager candidate");
                Box::pin(services.grant().create(institution.clone(), operation(), command(&input(&candidate, 1)))).await.expect("first domain manager");
                let message_id = message(&database, services.authentication(), &mut supplier, "13700000000").await;
                let Receipt::Registration(credential) = Box::pin(services.authentication().create_session(authentication(Uuid::new_v4(), message_id, "13700000000", CODE))).await.expect("third person's R1") else { panic!("registration credential"); };
                let registered = services.registration().register(crate::registration::Input::new(operation(), credential.id(), credential.token().text().parse().expect("proof"), "Other Manager".parse().expect("name"), "000000200002290088".parse().expect("id number"))).await.expect("third person's R2");
                let other = services.current().authenticate(current::Proof { id: registered.id(), token: registered.token().text().parse().expect("session token") }).await.expect("third actor");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13700000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("second domain manager candidate");
                let mut body = input(&candidate, 1);
                body["target"]["mobile"] = json!("13700000000");
                Box::pin(services.grant().create(institution, operation(), command(&body))).await.expect("second full Workforce role");
                assert!(matches!(services.grant().candidate(&manager, Entry::Domain, "13700000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await, Err(Error::Absent)));
                assert!(matches!(services.grant().candidate(&other, Entry::Domain, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await, Err(Error::Absent)));
                body["entry"] = json!("domain");
                body["target"]["revisionId"] = json!(Uuid::new_v4());
                assert!(matches!(Box::pin(services.grant().create(manager, operation(), command(&body))).await, Err(Error::Absent)), "the write also hides the target before disclosing stale confirmation");
                services.ready().await.expect("hidden target does not close admission");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.grants)=2 AND (SELECT count(*) FROM access.assignments)=2 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=2 AND NOT EXISTS(SELECT 1 FROM identity.persons WHERE sequence<>1)", &[]).await.expect("denials wrote no new facts").get::<_, bool>(0));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("domain target level deadline");
        }));
    }

    #[test]
    fn domain_admission_holds_its_grant_and_rechecks_identity_after_waiting() {
        async fn wait_for_lock(client: &tokio_postgres::Client, blocker: i32, query: &str) {
            let result = timeout(Duration::from_secs(3), async { loop {
                if client.query_one("WITH RECURSIVE blocked(pid) AS (SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) UNION SELECT backend.pid FROM pg_stat_activity backend JOIN blocked ON blocked.pid=ANY(pg_blocking_pids(backend.pid)) WHERE backend.datname=current_database()) SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid IN (SELECT pid FROM blocked) AND wait_event_type='Lock' AND query=$2)", &[&blocker,&query]).await.expect("actual wait graph including tuple-lock queues").get::<_, bool>(0) { break; }
                tokio::task::yield_now().await;
            }}).await;
            assert!(
                result.is_ok(),
                "operation reached its expected lock: {query}"
            );
        }

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for revoked in [false, true] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = register(&database, &services, &mut supplier).await;
                    let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("manager candidate");
                    let (source, _) = Box::pin(services.grant().create(institution, operation(), command(&input(&candidate, 1)))).await.expect("domain manager");
                    let candidate = services.grant().candidate(&manager, Entry::Domain, "13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("domain target");
                    let mut body = input(&candidate, 0);
                    body["entry"] = json!("domain"); body["target"]["mobile"] = json!("13900000000");
                    let operation = operation();
                    let blocker = database.connections("postgres").recover().await.expect("isolated grant lock holder");
                    blocker.client().batch_execute("BEGIN").await.expect("hold source");
                    if revoked {
                        blocker.client().query_one("SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE", &[&manager.account_id()]).await.expect("exclusive source account lock");
                    } else {
                        blocker.client().query_one("SELECT id FROM access.grants WHERE id=$1 FOR UPDATE", &[&source.id()]).await.expect("exclusive source grant lock");
                    }
                    let pid: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("blocker identity").get(0);
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let service = services.grant().clone(); let actor = manager.clone(); let command = command(&body);
                    if revoked {
                        let revocation = services.revocation().clone();
                        let logout = tokio::spawn(async move { revocation.revoke(manager, self::operation(), crate::identity::revocation::Scope::Current).await });
                        wait_for_lock(observer.client(), pid, "SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE").await;
                        let waiter = tokio::spawn(async move { service.create(actor, operation, command).await });
                        wait_for_lock(observer.client(), pid, "SELECT id FROM identity.accounts WHERE id=$1 FOR SHARE").await;
                        blocker.client().batch_execute("COMMIT").await.expect("release source");
                        timeout(Duration::from_secs(3), logout).await.expect("bounded logout").expect("logout task").expect("real logout precedes admission");
                        let result = timeout(Duration::from_secs(3), waiter).await.expect("bounded rejected waiter").expect("waiter");
                        let error = result.err().expect("revoked identity cannot admit");
                        assert!(matches!(error, Error::Current(crate::identity::current::Error::Invalid)), "{error:?}");
                        services.ready().await.expect("lost identity is not damaged storage");
                        services.close(); services.drain().await.expect("drained");
                    } else {
                        let waiter = tokio::spawn(async move { service.create(actor, operation, command).await });
                        wait_for_lock(observer.client(), pid, "SELECT id FROM access.grants WHERE person_id=$1 AND termination IS NULL ORDER BY id FOR SHARE").await;
                        waiter.abort(); assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                        services.close();
                        assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "accepted admission still owns its work while blocked");
                        blocker.client().batch_execute("COMMIT").await.expect("release source");
                        services.drain().await.expect("accepted work completes after the caller leaves");
                    }
                    blocker.terminate().await.expect("blocker ended");
                    let expected = if revoked { 1_i64 } else { 2_i64 };
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.grants),(SELECT count(*) FROM access.assignments),(SELECT count(*) FROM access.grants WHERE termination IS NULL),(SELECT sequence AS person_sequence FROM identity.persons WHERE id=$1)", &[&candidate.identity().person_id()]).await.expect("independent final graph");
                    for column in 0..3 { assert_eq!(row.get::<_, i64>(column), expected); }
                    assert_eq!(row.get::<_, i64>(3), 1, "admission preserves the Person creation state");
                    observer.terminate().await.expect("observer ended"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("domain grant coordination deadline");
            }
        }));
    }

    #[test]
    fn domain_admission_sql_requires_complete_source_and_selected_role_pairing() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(35),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = register(&database, &services, &mut supplier).await;
                    let candidate = services
                        .grant()
                        .candidate(
                            &institution,
                            Entry::Institution,
                            "13800000000".parse().expect("mobile"),
                            crate::pagination::Pagination::new(1, 20).expect("pagination"),
                        )
                        .await
                        .expect("manager candidate");
                    let (source, _) = Box::pin(services.grant().create(
                        institution,
                        operation(),
                        command(&input(&candidate, 1)),
                    ))
                    .await
                    .expect("real domain manager");
                    let target = services
                        .grant()
                        .candidate(
                            &manager,
                            Entry::Domain,
                            "13900000000".parse().expect("mobile"),
                            crate::pagination::Pagination::new(1, 20).expect("page"),
                        )
                        .await
                        .expect("target");
                    let target = Candidate {
                        roles: candidate.roles,
                        ..target
                    };
                    sql_admission(
                        &database,
                        &services,
                        &target,
                        source.subject().revision_id(),
                        Entry::Domain,
                    )
                    .await;
                    let owner = database
                        .connections("postgres")
                        .recover()
                        .await
                        .expect("isolated owner");
                    let observer = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("runtime verifier");
                    let user =
                        platform::database::User::new(database.user.clone()).expect("runtime user");
                    for (change, restore) in [
                        (
                            "ALTER FUNCTION access.lock_grant(uuid) SECURITY INVOKER",
                            "ALTER FUNCTION access.lock_grant(uuid) SECURITY DEFINER",
                        ),
                        (
                            "ALTER FUNCTION access.lock_grant(uuid) SET search_path = public",
                            "ALTER FUNCTION access.lock_grant(uuid) SET search_path = ''",
                        ),
                        (
                            "GRANT EXECUTE ON FUNCTION access.lock_grant(uuid) TO PUBLIC",
                            "REVOKE EXECUTE ON FUNCTION access.lock_grant(uuid) FROM PUBLIC",
                        ),
                    ] {
                        owner
                            .client()
                            .batch_execute(change)
                            .await
                            .expect("isolated function fault");
                        assert!(matches!(
                            crate::database::permissions::verify(observer.client(), &user).await,
                            Err(crate::database::Error::Permissions)
                        ));
                        owner
                            .client()
                            .batch_execute(restore)
                            .await
                            .expect("restore function contract");
                        crate::database::permissions::verify(observer.client(), &user)
                            .await
                            .expect("restored privileges");
                    }
                    observer.terminate().await.expect("observer ended");
                    owner.terminate().await.expect("owner ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    assert!(
                        timeout(Duration::from_millis(30), supplier.request())
                            .await
                            .is_err()
                    );
                    supplier.stop().await;
                }),
            )
            .await
            .expect("domain SQL boundary deadline");
        }));
    }

    #[test]
    fn current_role_is_self_scoped_complete_and_read_only() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for index in [0, 1] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = register(&database, &services, &mut supplier).await;
                    for actor in [&manager, &target] {
                        assert!(matches!(services.grant().read_role(actor).await, Err(Error::RoleAbsent)));
                    }
                    let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                    let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation(), command(&input(&candidate, index)))).await.expect("admitted with selected system role");
                    let observer = database.connections(&database.user).recover().await.expect("independent runtime observer");
                    let snapshot = "SELECT jsonb_build_object('persons',(SELECT jsonb_agg(to_jsonb(person) ORDER BY id) FROM identity.persons person),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session),'grants',(SELECT jsonb_agg(to_jsonb(admission) ORDER BY id) FROM access.grants admission),'assignments',(SELECT jsonb_agg(to_jsonb(assignment) ORDER BY id) FROM access.assignments assignment),'roles',(SELECT jsonb_agg(to_jsonb(role) ORDER BY id) FROM access.roles role),'changes',(SELECT jsonb_agg(to_jsonb(change) ORDER BY id) FROM access.changes change))::text";
                    let before: String = observer.client().query_one(snapshot, &[]).await.expect("business facts before reads").get(0);
                    let expected = observer.client().query_one("SELECT role.id,role.name,role.level,role.change_id,ARRAY(SELECT capability_id FROM access.permissions WHERE role_id=role.id ORDER BY capability_id COLLATE \"C\") AS permissions FROM access.grants state JOIN access.roles role ON role.id=state.role_id WHERE state.id=$1 AND state.termination IS NULL", &[&grant.id()]).await.expect("independent complete definition");
                    for _ in 0..2 {
                        let role = services.grant().read_role(&target).await.expect("own role requires no management permission");
                        assert_eq!(json!(role.id()), json!(expected.get::<_,Uuid>("id")));
                        assert_eq!(json!(role.name()), json!(expected.get::<_,String>("name")));
                        assert_eq!(i16::from(role.level()), expected.get::<_,i16>("level"));
                        assert_eq!(json!(role.change_id()), json!(expected.get::<_,Uuid>("change_id")));
                        assert_eq!(json!(role.permissions()), json!(expected.get::<_,Vec<String>>("permissions")));
                        assert_eq!(json!(role.kind()), if index == 0 { "default" } else { "full" });
                        assert_eq!(role.permissions().len(), if index == 0 { 0 } else { 11 });
                        assert!(matches!(services.grant().read_role(&manager).await, Err(Error::RoleAbsent)), "institutional authority does not imply Workforce membership");
                    }
                    assert_eq!(observer.client().query_one(snapshot, &[]).await.expect("facts after reads").get::<_,String>(0), before);
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    assert_eq!(json!(services.grant().read_role(&target).await.expect("role after restart").id()), json!(expected.get::<_,Uuid>("id")));
                    Box::pin(services.cancellation().cancel_account(manager, operation(), candidate.identity().id(), serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("real cancellation removes current membership");
                    assert!(matches!(services.grant().read_role(&target).await, Err(Error::Current(crate::identity::current::Error::Invalid))));
                    let replacement = register(&database, &services, &mut supplier).await;
                    assert!(matches!(services.grant().read_role(&replacement).await, Err(Error::RoleAbsent)), "new person cannot inherit the old membership");
                    assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL) AND (SELECT count(*) FROM access.grants)=1 AND (SELECT count(*) FROM access.assignments)=1", &[]).await.expect("cancellation preserves only historical admission").get::<_,bool>(0));
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("current role lifecycle deadline");
            }
        }));
    }

    #[test]
    fn current_role_uses_one_snapshot_and_withholds_unfinished_reads() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["snapshot", "retry", "lost commit", "ended driver", "cancel", "timeout"] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = if matches!(case, "snapshot" | "retry") { crate::access::role::tests::start_snapshot(authentication::proxied(&database, &proxy), &supplier).await } else { start(authentication::proxied(&database, &proxy), &supplier).await };
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = register(&database, &services, &mut supplier).await;
                    let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                    let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation(), command(&input(&candidate, 1)))).await.expect("admitted");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let expected = observer.client().query_one("SELECT role_id FROM access.grants WHERE id=$1 AND termination IS NULL", &[&grant.id()]).await.expect("current role before concurrent cancellation").get::<_,Uuid>(0);
                    proxy.arm(if case == "snapshot" { Fault::PausedReadEnd } else { Fault::StalledGrant });
                    let service = services.grant().clone(); let actor = target.clone();
                    let reader = tokio::spawn(async move { service.read_role(&actor).await });
                    let pid = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("before grant state validation or after the validated snapshot ends");
                    match case {
                        "snapshot" | "retry" => {
                            Box::pin(services.cancellation().cancel_account(manager, operation(), candidate.identity().id(), serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("cancellation after the read snapshot");
                        }
                        "lost commit" => proxy.arm(Fault::LostReadCommit),
                        "ended driver" => proxy.arm(Fault::CommitTermination),
                        "cancel" => reader.abort(),
                        "timeout" => {}
                        _ => unreachable!(),
                    }
                    if case != "timeout" { proxy.release(); }
                    let result = timeout(Duration::from_secs(6), reader).await.expect("bounded read and connection end");
                    if case == "timeout" { proxy.release(); }
                    if case == "cancel" {
                        assert!(result.is_err_and(|error| error.is_cancelled()));
                    } else {
                        let result = result.expect("reader task");
                        if case == "retry" {
                            assert!(matches!(result, Err(Error::Current(crate::identity::current::Error::Invalid))), "changed Account anchor restarts identity qualification");
                            services.ready().await.expect("legitimate concurrent cancellation does not close admission");
                        } else if matches!(case, "snapshot" | "ended driver") {
                            let role = result.expect("acknowledged read retains its complete snapshot");
                            assert_eq!(json!(role.id()), json!(expected)); assert_eq!(role.permissions().len(), 11);
                            if case == "snapshot" {
                                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL) AND EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$1)", &[&candidate.identity().person_id()]).await.expect("fresh snapshot sees committed cancellation").get::<_,bool>(0));
                                assert!(matches!(services.grant().read_role(&target).await, Err(Error::Current(crate::identity::current::Error::Invalid))));
                            }
                        } else {
                            let error = result.err().unwrap_or_else(|| panic!("{case}: no role disclosed after failed finalization"));
                            assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                        }
                    }
                    if matches!(case, "lost commit" | "ended driver") {
                        timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("selected finalization fault actually occurred"); proxy.release();
                    }
                    if matches!(case, "cancel" | "timeout") {
                        timeout(Duration::from_secs(3), async { loop {
                            let ended: bool = observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("actual backend lifetime").get(0);
                            if ended { break; } tokio::task::yield_now().await;
                        }}).await.expect("reader backend ended after cancellation or deadline");
                    }
                        services.ready().await.expect("ordinary read failure does not close admission");
                        if matches!(case, "snapshot" | "retry") {
                            assert!(matches!(services.grant().read_role(&target).await, Err(Error::Current(crate::identity::current::Error::Invalid))));
                        } else { assert_eq!(json!(services.grant().read_role(&target).await.expect("later independent read").id()), json!(expected)); }
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.grants", &[]).await.expect("read never creates another grant").get::<_,i64>(0), 1);
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); let stopped = context.stop().await;
                    stopped.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await; proxy.stop().await;
                })).await.unwrap_or_else(|error| panic!("{case} deadline: {error}"));
            }
        }));
    }

    #[test]
    fn current_role_rejects_damaged_membership_and_definition_after_identity_validation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["member", "person", "role pointer", "assignment", "grant", "future", "time range", "role", "definition", "permission", "revoked"] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = register(&database, &services, &mut supplier).await;
                    let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                    let body = input(&candidate, 1);
                    let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation(), command(&body))).await.expect("admitted");
                    if case == "revoked" {
                        Box::pin(services.revocation().revoke(target.clone(), operation(), crate::identity::revocation::Scope::Current)).await.expect("real session revocation");
                    }
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                    let sql = match case {
                        "member" | "revoked" => "DELETE FROM access.grants WHERE id=$1",
                        "person" => "UPDATE access.grants SET person_id=gen_random_uuid() WHERE id=$1",
                        "role pointer" => "UPDATE access.grants SET role_id=(SELECT id FROM access.roles WHERE scope='workforce' AND level=1) WHERE id=$1",
                        "assignment" => "DELETE FROM access.assignments WHERE grant_id=$1",
                        "grant" => "DELETE FROM access.grants WHERE id=$1",
                        "future" => "UPDATE access.grants SET granted_at=clock_timestamp()+interval '1 day',changed_at=clock_timestamp()+interval '1 day' WHERE id=$1",
                        "time range" => "UPDATE access.grants SET granted_at='294000-01-01 00:00:00+00',changed_at='294000-01-01 00:00:00+00' WHERE id=$1",
                        "role" => "DELETE FROM access.roles WHERE id=(SELECT role_id FROM access.grants WHERE id=$1)",
                        "definition" => "DELETE FROM access.changes WHERE id=(SELECT role.change_id FROM access.roles role JOIN access.grants state ON state.role_id=role.id WHERE state.id=$1)",
                        "permission" => "UPDATE access.roles SET permissions=array_replace(permissions,'workforce.roles','unexpected.permission') WHERE id=(SELECT role_id FROM access.grants WHERE id=$1)",
                        _ => unreachable!(),
                    };
                    assert_eq!(owner.client().execute(sql, &[&grant.id()]).await.expect("exact corruption"), 1);
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let result = services.grant().read_role(&target).await;
                    let error = if matches!(case, "assignment" | "definition") {
                        assert_eq!(serde_json::to_value(result.expect("current eligibility does not reconstruct historical definitions").id()).expect("role ID"), body["role"]["id"]);
                        services.ready().await.expect("unconsumed historical records do not close admission");
                        let damaged = if case=="definition" {
                            crate::history::inspect::<crate::access::role::state::State>(owner.client(), services.app().keys(), context.sample().await.expect("startup sample")).await.is_err()
                        } else {
                            services.grant().verify(owner.client()).await.is_err()
                        };
                        assert!(damaged, "startup rejects missing retained records");
                        services.grant().read(&target, grant.id()).await.err().expect("management delivery consumes its fixed first assignment and definition")
                    } else {
                        result.err().expect("failed state read does not disclose role")
                    };
                    if case == "revoked" {
                        assert!(matches!(error, Error::Current(crate::identity::current::Error::Invalid)), "identity rejection precedes damaged membership access");
                        services.ready().await.expect("unread damage did not masquerade as an integrity check");
                    } else {
                        assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                        if case == "permission" {
                            assert!(matches!(&error, Error::Role(crate::access::role::Error::History(crate::history::Error::Decode(_)))), "{error:?}");
                        }
                        assert!(services.ready().await.is_err(), "{case}: corrupt facts close admission");
                        let later = Box::pin(services.grant().create(manager, operation(), command(&body))).await.err().expect("closed admission rejects later work");
                        assert!(matches!(later, Error::Command(crate::kernel::Error::Unavailable(ref fault)) if matches!(fault.kind(),crate::kernel::FaultKind::Closed)), "{case}: {later:?}");
                    }
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.grants", &[]).await.expect("no replacement admission").get::<_,i64>(0), i64::from(!matches!(case,"grant" | "member" | "revoked")));
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("{case} deadline: {error}"));
            }
        }));
    }

    #[test]
    fn person_state_locators_reject_missing_retained_grants() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for surface in ["role", "grant", "candidate"] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = register(&database, &services, &mut supplier).await;
                    let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                    let body = input(&candidate, 0);
                    let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation(), command(&body))).await.expect("real current admission");
                    Box::pin(services.grant().terminate(target.clone(), operation(), grant.id())).await.expect("first Grant ends");
                    let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("confirmed re-admission");
                    let body = input(&candidate, 0);
                    let (latest, assignment) = Box::pin(services.grant().create(manager.clone(), operation(), command(&body))).await.expect("new Grant has its own complete state");
                    services.grant().read_role(&target).await.expect("complete current role before damage");
                    let owner = database.connections("postgres").recover().await.expect("isolated damage owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated deletion of a retained state");
                    assert_eq!(owner.client().execute("DELETE FROM access.grants WHERE id=$1", &[&grant.id()]).await.expect("remove only the older Grant state"), 1);
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let facts = "SELECT (SELECT sequence AS person_sequence FROM identity.persons WHERE id=$1)=1 AND EXISTS(SELECT 1 FROM access.grants state JOIN access.assignments assignment ON assignment.id=state.assignment_id AND assignment.grant_id=state.id AND assignment.person_id=state.person_id AND assignment.role_id=state.role_id WHERE state.id=$2 AND state.person_id=$1 AND state.sequence=1 AND assignment.sequence=1 AND state.termination IS NULL) AND NOT EXISTS(SELECT 1 FROM access.grants WHERE id=$3) AND EXISTS(SELECT 1 FROM access.assignments WHERE grant_id=$3) AND EXISTS(SELECT 1 FROM access.terminations WHERE id=$3) AND NOT EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$1)";
                    assert!(owner.client().query_one(facts, &[&candidate.identity().person_id(),&latest.id(),&grant.id()]).await.expect("complete current state and orphaned older records").get::<_,bool>(0));
                    let result = match surface {
                        "role" => {
                            assert_eq!(services.grant().read_role(&target).await.expect("the current role consumes only its current Grant").id(), assignment.role().id());
                            services.ready().await.expect("unconsumed older state does not close admission");
                            services.grant().read_latest(&target).await.map(|_| ())
                        },
                        "grant" => services.grant().read_latest(&target).await.map(|_| ()),
                        "candidate" => services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.map(|_| ()),
                        _ => unreachable!(),
                    };
                    let error = result.expect_err("an older orphan prevents person-scoped delivery");
                    assert_eq!(error.failure(), super::Failure::Unavailable, "{surface}: {error:?}");
                    assert!(services.ready().await.is_err());
                    let error = Box::pin(services.grant().create(manager, operation(), command(&body))).await.err().expect("later work rejected");
                    assert!(matches!(error, Error::Command(crate::kernel::Error::Unavailable(fault)) if matches!(fault.kind(), crate::kernel::FaultKind::Closed)));
                    assert!(services.grant().verify(owner.client()).await.is_err(), "startup rejects the orphaned older records");
                    assert!(owner.client().query_one(facts, &[&candidate.identity().person_id(),&latest.id(),&grant.id()]).await.expect("read and rejected work leave the damaged facts untouched").get::<_,bool>(0));
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.grants", &[]).await.expect("no repair or replacement grant").get::<_,i64>(0), 1);
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("{surface} deadline: {error}"));
            }
        }));
    }

    #[test]
    fn latest_grant_is_self_scoped_read_only_and_retains_admission_identity() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let target = register(&database, &services, &mut supplier).await;
                for actor in [&manager, &target] {
                    assert!(matches!(services.grant().read_latest(actor).await, Err(Error::Missing)));
                }
                let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation(), command(&input(&candidate, 0)))).await.expect("ordinary member admitted");
                let observer = database.connections(&database.user).recover().await.expect("runtime observer");
                let snapshot = "SELECT jsonb_build_object('persons',(SELECT jsonb_agg(to_jsonb(person) ORDER BY id) FROM identity.persons person),'revisions',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM identity.revisions revision),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session),'grants',(SELECT jsonb_agg(to_jsonb(admission) ORDER BY id) FROM access.grants admission),'assignments',(SELECT jsonb_agg(to_jsonb(assignment) ORDER BY id) FROM access.assignments assignment))::text";
                let before: String = observer.client().query_one(snapshot, &[]).await.expect("business facts before reads").get(0);
                for _ in 0..2 {
                    let latest = services.grant().read_latest(&target).await.expect("ordinary member reads without manager permission");
                    assert_eq!(latest.id(), grant.id());
                    assert_eq!(latest.subject().person_id(), candidate.identity().person_id());
                    assert_eq!(latest.subject().revision_id(), candidate.identity().revision_id());
                    assert_eq!(latest.subject().name(), "T*");
                    assert_eq!(latest.operator().revision_id(), grant.operator().revision_id());
                    assert_eq!(latest.granted_at(), grant.granted_at());
                    assert_eq!(latest.operation_id(), grant.operation_id());
                    assert_eq!(latest.sequence(), 1);
                    assert_eq!(latest.status(), Status::Active);
                    assert!(matches!(services.grant().read_latest(&manager).await, Err(Error::Missing)), "manager cannot see someone else's grant through the self entry");
                }
                let after: String = observer.client().query_one(snapshot, &[]).await.expect("business facts after reads").get(0);
                assert_eq!(before, after, "reads neither refresh sessions nor write admission or identity facts");
                let correction = services.correction().candidate_by_mobile(&manager, "13800000000".parse().expect("mobile")).await.expect("correction confirmation");
                Box::pin(services.correction().correct(manager.clone(), operation(), serde_json::from_value(json!({"target":correction.target(),"name":"Changed Holder","idNumber":"00000020000229007X"})).expect("typed correction"))).await.expect("later name correction");
                let latest = services.grant().read_latest(&target).await.expect("fixed original identity after correction");
                assert_eq!(latest.subject().revision_id(), grant.subject().revision_id());
                assert_eq!(latest.subject().name(), "T*");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                assert_eq!(services.grant().read_latest(&target).await.expect("original session after restart").id(), grant.id());
                let current = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("current target");
                Box::pin(services.cancellation().cancel_account(manager, operation(), current.identity().id(), serde_json::from_value(json!({"personId":current.identity().person_id(),"revisionId":current.identity().revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("real account cancellation");
                assert!(matches!(services.grant().read_latest(&target).await, Err(Error::Current(crate::identity::current::Error::Invalid))));
                let replacement = register(&database, &services, &mut supplier).await;
                assert!(matches!(services.grant().read_latest(&replacement).await, Err(Error::Missing)), "new person using the same mobile does not inherit the old grant");
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.grants)=1 AND (SELECT count(*) FROM access.assignments)=1 AND NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL) AND EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$1)", &[&candidate.identity().person_id()]).await.expect("retained historical grant").get::<_,bool>(0));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("self grant lifecycle deadline");
        }));
    }

    #[test]
    fn latest_grant_uses_one_snapshot_and_withholds_unfinished_reads() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["snapshot", "retry", "lost commit", "ended driver", "cancel", "timeout"] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = if matches!(case, "snapshot" | "retry") { crate::access::role::tests::start_snapshot(authentication::proxied(&database, &proxy), &supplier).await } else { start(authentication::proxied(&database, &proxy), &supplier).await };
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = register(&database, &services, &mut supplier).await;
                    let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                    let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation(), command(&input(&candidate, 0)))).await.expect("admitted");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    proxy.arm(if case == "snapshot" { Fault::PausedReadEnd } else { Fault::StalledGrant });
                    let service = services.grant().clone(); let actor = target.clone();
                    let reader = tokio::spawn(async move { service.read_latest(&actor).await });
                    let pid = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("before grant state validation or after the validated snapshot ends");
                    match case {
                        "snapshot" | "retry" => {
                            Box::pin(services.cancellation().cancel_account(manager, operation(), candidate.identity().id(), serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("cancellation after the read snapshot");
                        }
                        "lost commit" => proxy.arm(Fault::LostReadCommit),
                        "ended driver" => proxy.arm(Fault::CommitTermination),
                        "cancel" => reader.abort(),
                        "timeout" => {}
                        _ => unreachable!(),
                    }
                    if case != "timeout" { proxy.release(); }
                    let result = timeout(Duration::from_secs(6), reader).await.expect("bounded read and connection end");
                    if case == "timeout" { proxy.release(); }
                    if case == "cancel" {
                        assert!(result.is_err_and(|error| error.is_cancelled()));
                    } else {
                        let result = result.expect("reader task");
                        if case == "retry" {
                            assert!(matches!(result, Err(Error::Current(crate::identity::current::Error::Invalid))), "changed Account anchor restarts identity qualification");
                            services.ready().await.expect("legitimate concurrent cancellation does not close admission");
                        } else if matches!(case, "snapshot" | "ended driver") {
                            let latest = result.expect("acknowledged read retains its complete snapshot");
                            assert_eq!(latest.id(), grant.id()); assert_eq!(latest.status(), Status::Active);
                            if case == "snapshot" {
                                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL) AND EXISTS(SELECT 1 FROM access.grants WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$2)", &[&grant.id(), &candidate.identity().person_id()]).await.expect("fresh snapshot sees committed cancellation").get::<_,bool>(0));
                                assert!(matches!(services.grant().read_latest(&target).await, Err(Error::Current(crate::identity::current::Error::Invalid))));
                            }
                        } else {
                            let error = result.err().unwrap_or_else(|| panic!("{case}: no grant disclosed after failed finalization"));
                            assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                        }
                    }
                    if matches!(case, "lost commit" | "ended driver") {
                        timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("selected finalization fault actually occurred");
                        proxy.release();
                    }
                    if case == "cancel" || case == "timeout" {
                        timeout(Duration::from_secs(3), async { loop {
                            let ended: bool = observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("actual backend lifetime").get(0);
                            if ended { break; } tokio::task::yield_now().await;
                        }}).await.expect("reader backend ended after cancellation or deadline");
                    }
                        services.ready().await.expect("ordinary read failure does not close admission");
                        if matches!(case, "snapshot" | "retry") {
                            assert!(matches!(services.grant().read_latest(&target).await, Err(Error::Current(crate::identity::current::Error::Invalid))));
                        } else { assert_eq!(services.grant().read_latest(&target).await.expect("later independent read").id(), grant.id()); }
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.grants", &[]).await.expect("read never creates another grant").get::<_,i64>(0), 1);
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); let stopped = context.stop().await;
                    stopped.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await; proxy.stop().await;
                })).await.unwrap_or_else(|error| panic!("{case} deadline: {error}"));
            }
        }));
    }

    #[test]
    fn latest_grant_rejects_damaged_history_after_validating_the_original_session() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["member", "assignment", "grant", "future", "subject", "operator", "revoked"] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = register(&database, &services, &mut supplier).await;
                    let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                    let (grant, _) = Box::pin(services.grant().create(manager, operation(), command(&input(&candidate, 0)))).await.expect("admitted");
                    if case == "revoked" {
                        Box::pin(services.revocation().revoke(target.clone(), operation(), crate::identity::revocation::Scope::Current)).await.expect("real session revocation");
                    }
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                    let sql = match case {
                        "member" | "revoked" => "DELETE FROM access.grants WHERE id=$1",
                        "assignment" => "DELETE FROM access.assignments WHERE grant_id=$1",
                        "grant" => "DELETE FROM access.grants WHERE id=$1",
                        "future" => "UPDATE access.grants SET granted_at=statement_timestamp()+interval '1 day',changed_at=statement_timestamp()+interval '1 day' WHERE id=$1",
                        "subject" => "UPDATE identity.revisions SET name=id_number WHERE id=(SELECT revision_id FROM access.grants WHERE id=$1)",
                        "operator" => "UPDATE identity.revisions SET id_number=set_byte(id_number,octet_length(id_number)-1,get_byte(id_number,octet_length(id_number)-1)#1) WHERE id=(SELECT operator_id FROM access.grants WHERE id=$1)",
                        _ => unreachable!(),
                    };
                    assert_eq!(owner.client().execute(sql, &[&grant.id()]).await.expect("exact corruption"), 1);
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    if case == "assignment" {
                        assert_eq!(services.grant().read_latest(&target).await.expect("latest state does not consume the first assignment").id(), grant.id());
                        services.ready().await.expect("unread record does not close admission");
                        assert!(services.grant().verify(owner.client()).await.is_err(), "startup rejects the missing first record");
                        owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
                        return;
                    }
                    let error = services.grant().read_latest(&target).await.err().expect("failed read does not disclose grant");
                    if case == "revoked" {
                        assert!(matches!(error, Error::Current(crate::identity::current::Error::Invalid)), "identity rejection precedes damaged grant access");
                        services.ready().await.expect("unread damage did not masquerade as an integrity check");
                    } else {
                        assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                        assert!(services.ready().await.is_err(), "{case}: corrupt facts close admission");
                    }
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("{case} deadline: {error}"));
            }
        }));
    }

    #[test]
    fn institution_admission_preserves_one_complete_original_after_cancellation_and_restart() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let target = register(&database, &services, &mut supplier).await;
                let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                assert!(candidate.suspension_id().is_none());
                assert!(candidate.grant().is_none());
                assert_eq!(candidate.roles().len(), 2);
                assert_eq!(candidate.roles()[0].permissions().len(), 11);
                assert!(candidate.roles()[1].permissions().is_empty());
                assert_eq!(candidate.identity().name(), "T*");
                assert_eq!(candidate.identity().suffix(), "007X");
                assert!(matches!(services.grant().candidate(&target, Entry::Institution, "13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await, Err(Error::Role(crate::access::role::Error::Denied))));
                for mobile in ["13900000000", "13700000000"] {
                    assert!(matches!(services.grant().candidate(&manager, Entry::Institution, mobile.parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await, Err(Error::Absent)));
                }
                let body = input(&candidate, 1);
                let operation = operation();
                let (grant, assignment) = Box::pin(services.grant().create(manager.clone(), operation, command(&body))).await.expect("institution grants full Workforce role without its own membership");
                assert_eq!(grant.status(), Status::Active);
                assert_eq!(grant.sequence(), 1);
                assert_eq!(assignment.sequence(), 1);
                assert_eq!(assignment.grant_id(), grant.id());
                assert_eq!(assignment.role().id(), candidate.roles()[0].id());
                let observer = database.connections(&database.user).recover().await.expect("runtime observer");
                assert!(observer.client().query_one("SELECT person.sequence=1 AND (SELECT count(*) FROM access.grants)=1 AND (SELECT count(*) FROM access.assignments)=1 AND admission.sequence=1 AND admission.assignment_id=$2 AND admission.role_id=$3 AND admission.termination IS NULL AND assigned.transaction_id=admission.transaction_id FROM identity.persons person JOIN access.grants admission ON admission.person_id=person.id JOIN access.assignments assigned ON assigned.id=admission.assignment_id WHERE person.id=$1", &[&candidate.identity().person_id(), &assignment.id(), &assignment.role().id()]).await.expect("independent complete graph").get::<_,bool>(0));
                let (replayed, repeated) = Box::pin(services.grant().create(manager.clone(), operation, command(&body))).await.expect("original result");
                assert_eq!(replayed.id(), grant.id());
                assert_eq!(repeated.id(), assignment.id());
                for path in ["mobile", "accountId", "personId", "revisionId", "assignmentId"] {
                    let mut altered = body.clone();
                    altered["target"][path] = if path == "mobile" { json!("13700000000") } else { json!(Uuid::new_v4()) };
                    assert!(matches!(Box::pin(services.grant().create(manager.clone(), operation, command(&altered))).await, Err(Error::Mismatch)), "{path}");
                }
                let fresh = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("active confirmation");
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), self::operation(), command(&input(&fresh, 1)))).await, Err(Error::AlreadyAuthorized)));
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), self::operation(), command(&body))).await, Err(Error::TargetChanged)));
                let cancelled = Box::pin(services.cancellation().cancel_account(manager.clone(), self::operation(), candidate.identity().id(), serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"})).expect("cancellation input"))).await.expect("cancellation removes membership in the same transaction");
                assert!(cancelled.cancelled_at() >= grant.granted_at());
                let (ended, fixed) = Box::pin(services.grant().create(manager.clone(), operation, command(&body))).await.expect("original result after target cancellation");
                assert_eq!(ended.id(), grant.id());
                assert_eq!(ended.status(), Status::Terminated);
                assert_eq!(fixed.id(), assignment.id());
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.grants)=1 AND (SELECT count(*) FROM access.assignments)=1 AND NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL) AND EXISTS(SELECT 1 FROM access.grants admission JOIN access.terminations ended ON ended.id=admission.id WHERE admission.person_id=$2 AND admission.sequence=2 AND admission.termination='cancellation' AND ended.cancellation_id=$3 AND ended.sequence=2) AND EXISTS(SELECT 1 FROM identity.accounts WHERE id=$1 AND person_id=$2 AND cancellation_id=$3 AND mobile IS NULL AND id_number IS NULL AND suspension_id IS NULL) AND (SELECT sequence AS person_sequence FROM identity.persons WHERE id=$2)=2", &[&candidate.identity().id(), &candidate.identity().person_id(), &cancelled.id()]).await.expect("permanent facts, Account tombstone and independent Grant sequence").get::<_,bool>(0));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (ended, fixed) = Box::pin(services.grant().create(manager, operation, command(&body))).await.expect("retained original on restart");
                assert_eq!(ended.status(), Status::Terminated);
                assert_eq!(fixed.id(), assignment.id());
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
            })).await.expect("admission lifecycle deadline");
        }));
    }

    #[test]
    fn admission_rechecks_pauses_roles_and_concurrent_confirmation_under_the_account_lock() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                register(&database, &services, &mut supplier).await;
                let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                let body = input(&candidate, 0);
                let institution = services.role().read_current(&manager).await.expect("institution role");
                let mut wrong = body.clone(); wrong["role"] = json!({"id":institution.id(),"changeId":institution.change_id()});
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), operation(), command(&wrong))).await, Err(Error::RoleChanged)));
                wrong = body.clone(); wrong["role"]["changeId"] = json!(Uuid::new_v4());
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), operation(), command(&wrong))).await, Err(Error::RoleChanged)));
                let confirmation = json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"});
                let suspended = Box::pin(services.suspension().suspend(manager.clone(), operation(), candidate.identity().id(), serde_json::from_value(confirmation.clone()).expect("pause confirmation"))).await.expect("account paused");
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), operation(), command(&body))).await, Err(Error::TargetChanged)));
                let paused = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("paused candidate");
                assert_eq!(paused.suspension_id(), Some(suspended.id()));
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), operation(), command(&input(&paused, 0)))).await, Err(Error::AccountNotReady)));
                let mut confirmation = confirmation;
                confirmation["suspensionId"] = json!(suspended.id());
                let resumed=Box::pin(services.suspension().resume(manager.clone(), operation(), candidate.identity().id(), serde_json::from_value(confirmation).expect("resume confirmation"))).await.expect("account resumed");
                assert_eq!(resumed.id(),suspended.id());
                let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("resumed candidate");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let gate = database.connections("postgres").recover().await.expect("isolated row gate");
                gate.client().batch_execute("BEGIN").await.expect("gate transaction");
                gate.client().execute("SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE", &[&candidate.identity().id()]).await.expect("hold target account");
                let gate_pid:i32=gate.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("account gate PID").get(0);
                let spawn = |input| { let service = services.grant().clone(); let actor = manager.clone(); tokio::spawn(async move { service.create(actor, operation(), input).await }) };
                let first = spawn(command(&input(&candidate, 0)));
                let second = spawn(command(&input(&candidate, 1)));
                timeout(Duration::from_secs(3), async { loop {
                    let row = observer.client().query_one("WITH RECURSIVE blocked(pid) AS (SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_catalog.pg_blocking_pids(pid)) UNION SELECT backend.pid FROM pg_catalog.pg_stat_activity backend JOIN blocked ON blocked.pid=ANY(pg_catalog.pg_blocking_pids(backend.pid)) WHERE backend.datname=current_database()) SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename=current_user AND pid IN(SELECT pid FROM blocked) AND wait_event_type='Lock' AND query='SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE'", &[&gate_pid]).await.expect("two accepted writes queued behind the exact target account lock");
                    if row.get::<_,i64>(0)==2 { break; } tokio::task::yield_now().await;
                }}).await.expect("account serializes both writes");
                gate.client().batch_execute("COMMIT").await.expect("release gate"); gate.terminate().await.expect("gate ended");
                let (first, second) = timeout(Duration::from_secs(5), async { (first.await.expect("first"), second.await.expect("second")) }).await.expect("both writes finish");
                assert!(matches!((&first, &second), (Ok(_), Err(Error::TargetChanged)) | (Err(Error::TargetChanged), Ok(_))));
                let (grant,assignment)=first.as_ref().or(second.as_ref()).expect("one confirmed admission");
                assert_eq!(grant.sequence(),1);assert_eq!(assignment.sequence(),1);
                let facts=observer.client().query_one(
                    "SELECT EXISTS(SELECT 1 FROM identity.persons WHERE id=$1 AND sequence=1)
                            AND (SELECT count(*) FROM access.grants)=1 AND (SELECT count(*) FROM access.assignments)=1
                            AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=1
                            AND EXISTS(SELECT 1 FROM access.grants WHERE id=$4 AND person_id=$1 AND sequence=1)
                            AND EXISTS(SELECT 1 FROM access.assignments WHERE id=$5 AND grant_id=$4 AND person_id=$1 AND sequence=1)
                            AND EXISTS(SELECT 1 FROM access.grants WHERE id=$4 AND person_id=$1 AND assignment_id=$5 AND termination IS NULL) AS personal,
                        EXISTS(SELECT 1 FROM identity.accounts account
                            JOIN identity.creations creation ON creation.id=account.id AND creation.person_id=account.person_id
                            JOIN identity.suspensions suspension ON suspension.account_id=account.id AND suspension.person_id=account.person_id
                            JOIN identity.resumptions resumption ON resumption.id=suspension.id AND resumption.account_id=account.id AND resumption.person_id=account.person_id
                            WHERE account.id=$2 AND account.person_id=$1 AND account.cancellation_id IS NULL AND account.suspension_id IS NULL
                                AND creation.sequence=1 AND account.created_at=creation.created_at
                                AND account.sequence=3 AND account.version=3 AND suspension.id=$3 AND suspension.sequence=2 AND resumption.sequence=3
                                AND account.changed_at=resumption.resumed_at)
                            AND (SELECT count(*) FROM identity.suspensions WHERE account_id=$2)=1
                            AND (SELECT count(*) FROM identity.resumptions WHERE account_id=$2)=1 AS account",
                    &[&candidate.identity().person_id(),&candidate.identity().id(),&suspended.id(),&grant.id(),&assignment.id()],
                ).await.expect("one Grant event leaves the separate Account suspension and resumption sequence intact");
                for column in ["personal","account"] {assert!(facts.get::<_,bool>(column),"{column}");}
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("qualification deadline");
        }));
    }

    #[test]
    fn admission_resolves_commit_faults_and_retains_work_after_the_waiter_leaves() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for entry in [Entry::Institution, Entry::Domain] {
            for fault in [Fault::LostCommit, Fault::RolledBack, Fault::UnknownCommit, Fault::ReadCommitTermination, Fault::DelayedCommit] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let member = register(&database, &services, &mut supplier).await;
                    let (manager, mobile, index, existing) = match entry {
                        Entry::Institution => (institution, "13800000000", 1, 0_i64),
                        Entry::Domain => {
                            let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("manager candidate");
                            Box::pin(services.grant().create(institution, self::operation(), command(&input(&candidate, 1)))).await.expect("real domain manager");
                            (member, "13900000000", 0, 1_i64)
                        }
                    };
                    let candidate = services.grant().candidate(&manager, entry, mobile.parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                    let mut body = input(&candidate, index);
                    body["entry"] = json!(entry.as_ref());
                    body["target"]["mobile"] = json!(mobile);
                    let input = command(&body); let operation = operation();
                    let cancelled = matches!(fault, Fault::DelayedCommit);
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    let unknown = matches!(fault, Fault::UnknownCommit);
                    let terminated = matches!(fault, Fault::ReadCommitTermination);
                    proxy.arm(if terminated { Fault::DelayedCommit } else { fault });
                    let service = services.grant().clone(); let actor = manager.clone();
                    let waiter = tokio::spawn(async move { service.create(actor, operation, input).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("fault reached business commit");
                    if terminated { proxy.arm(Fault::ReadCommitTermination); }
                    if cancelled {
                        waiter.abort(); let _ = waiter.await;
                        services.close();
                        assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "waiter cancellation cannot release running work");
                        proxy.release(); services.drain().await.expect("pending accepted write drained");
                    } else {
                        proxy.release();
                        let result = timeout(Duration::from_secs(15), waiter).await.expect("bounded outcome").expect("waiter");
                        if rolled_back {
                            let error = result.err().expect("known rollback is a failure");
                            assert!(matches!(error, Error::Command(crate::kernel::Error::Unavailable(_))), "{error:?}");
                        } else if unknown {
                            let error = result.err().expect("unknown commit is a failure");
                            assert!(matches!(error, Error::Command(crate::kernel::Error::Unknown(_))), "{error:?}");
                        } else {
                            let (grant, _) = result.expect("known committed result"); assert_eq!(grant.status(), Status::Active);
                        }
                        services.close(); services.drain().await.expect("drained");
                    }
                    context.stop().await.expect("stopped");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    // Recovery may fail before the proxy's original backend completes.
                    // Cross its account lock before stopping the proxy or inspecting the result.
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE", &[&candidate.identity().id()]).await.expect("original backend released the account lock");
                    let expected = if rolled_back { 0_i64 } else { 1_i64 };
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.grants) AS grants,(SELECT count(*) FROM access.assignments) AS assignments,(SELECT count(*) FROM access.grants WHERE termination IS NULL) AS members,sequence AS person_sequence FROM identity.persons WHERE id=$1", &[&candidate.identity().person_id()]).await.expect("durable result after driver termination");
                    for column in ["grants", "assignments", "members"] { assert_eq!(row.get::<_,i64>(column), existing + expected, "{entry:?}: {column}"); }
                    assert_eq!(row.get::<_,i64>("person_sequence"), 1);
                    observer.client().batch_execute("COMMIT").await.expect("observation ended");
                    observer.terminate().await.expect("observer ended");
                    proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (grant, _) = Box::pin(services.grant().create(manager, operation, command(&body))).await.expect("explicit request reads original or follows proven rollback");
                    assert_eq!(grant.sequence(), 1);
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("fault scenario deadline");
            }
            }
        }));
    }

    #[test]
    fn runtime_sql_requires_complete_admission_and_exact_permissions() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                register(&database, &services, &mut supplier).await;
                let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                let observer = database.connections(&database.user).recover().await.expect("runtime observer");
                let operator: crate::identity::person::revision::Id = observer.client().query_one("SELECT person.revision_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1", &[&manager.session_id()]).await.expect("actual manager revision").get(0);
                sql_admission(&database, &services, &candidate, operator, Entry::Institution).await;
                let owner = database.connections("postgres").recover().await.expect("isolated privilege fault owner");
                let user = platform::database::User::new(database.user.clone()).expect("runtime user");
                for sql in ["REVOKE SELECT ON access.grants FROM", "REVOKE UPDATE ON access.grants FROM", "GRANT DELETE ON access.grants TO", "REVOKE INSERT ON access.assignments FROM", "GRANT UPDATE ON access.assignments TO"] {
                    owner.client().batch_execute("BEGIN").await.expect("disposable privilege change");
                    owner.client().batch_execute(&format!("{sql} {}", database.user)).await.expect("specific permission mutation");
                    assert!(matches!(crate::database::permissions::verify(owner.client(), &user).await, Err(crate::database::Error::Permissions)), "{sql}");
                    owner.client().batch_execute("ROLLBACK").await.expect("restore exact permissions");
                }
                crate::database::permissions::verify(observer.client(), &user).await.expect("original permissions restored");
                let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation(), command(&input(&candidate, 0)))).await.expect("complete graph still accepted");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("explicit corruption fixture");
                owner.client().execute("DELETE FROM access.grants WHERE id=$1", &[&grant.id()]).await.expect("remove required current member");
                owner.client().batch_execute("COMMIT").await.expect("damage persisted");
                assert!(services.grant().verify(observer.client()).await.is_err(), "startup rejects the orphaned records");
                assert!(services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.is_err(), "anchored missing Grant is not reliable absence");
                assert!(services.ready().await.is_err(), "unreliable facts close admission");
                owner.terminate().await.expect("owner ended"); observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("SQL integrity deadline");
        }));
    }
}

#[cfg(test)]
mod recovery_tests {
    //! Cross-object Grant projections settle together with their original command.

    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use fixture::database_proxy::{Fault, Proxy};
    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::{
            Entry,
            appointment::tests::{register, register_numbered},
            grant::{self, tests::admit},
            role::{
                self, Kind,
                tests::{actor, start},
            },
        },
        database::transaction,
        identity::{
            authentication::{self, Database, Supplier, server},
            operation,
        },
        kernel::{self, operation::Family},
        pagination::Pagination,
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    const FACTS: &str = "SELECT jsonb_build_array(
        (SELECT jsonb_agg(value ORDER BY id) FROM access.roles value),
        (SELECT jsonb_agg(value ORDER BY id) FROM access.changes value),
        (SELECT jsonb_agg(value ORDER BY id) FROM access.grants value),
        (SELECT jsonb_agg(value ORDER BY id) FROM access.losses value),
        (SELECT jsonb_agg(value ORDER BY family,operation_id) FROM access.completions value))::text";

    #[test]
    fn cancellation_grant_projection_shares_commit_and_rollback_with_account() {
        use crate::cancellation::tests::{
            assert_cancelled_account, before_cancellation, confirmation, management,
        };

        const GRAPH: &str = "SELECT jsonb_build_array(
            (SELECT jsonb_agg(value ORDER BY family,operation_id) FROM identity.completions value),
            (SELECT jsonb_agg(value ORDER BY id) FROM identity.accounts value),
            (SELECT jsonb_agg(value ORDER BY id) FROM identity.persons value),
            (SELECT jsonb_agg(value ORDER BY id) FROM identity.sessions value),
            (SELECT jsonb_agg(value ORDER BY id) FROM identity.issuances value),
            (SELECT jsonb_agg(value ORDER BY id) FROM identity.revocations value),
            (SELECT jsonb_agg(value ORDER BY id) FROM identity.handoffs value),
            (SELECT jsonb_agg(value ORDER BY id) FROM identity.receipts value),
            (SELECT jsonb_agg(value ORDER BY id) FROM identity.projections value),
            (SELECT jsonb_agg(value ORDER BY id) FROM identity.avatars value),
            (SELECT jsonb_agg(value ORDER BY id) FROM identity.cancellations value),
            (SELECT jsonb_agg(value ORDER BY person_id,sequence) FROM identity.changes value),
            (SELECT jsonb_agg(value ORDER BY id) FROM access.holders value),
            (SELECT jsonb_agg(value ORDER BY id) FROM access.projections value),
            (SELECT jsonb_agg(value ORDER BY id) FROM access.grants value),
            (SELECT jsonb_agg(value ORDER BY id) FROM access.terminations value))::text";
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for managed in [false, true] {
                for case in ["lost commit", "unknown commit", "account write"] {
                    timeout(Duration::from_secs(55), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                        let manager = actor(&database, &services, &mut supplier).await;
                        let holder = register(&database, &services, &mut supplier).await;
                        let ended = Box::pin(admit(&services, &manager, Entry::Institution, "13800000000", Kind::Default)).await;
                        Box::pin(services.grant().terminate(holder.clone(), operation(), ended.id())).await.expect("old self termination");
                        let current = Box::pin(admit(&services, &manager, Entry::Institution, "13800000000", Kind::Default)).await;
                        let account = services.current().read(&holder).await.expect("target identity");
                        let observer = database.connections(&database.user).recover().await.expect("independent observer");
                        let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                        let before = before_cancellation(observer.client(), &account).await;
                        let graph: String = observer.client().query_one(GRAPH, &[]).await.expect("original complete graph").get(0);
                        let old: String = observer.client().query_one("SELECT jsonb_build_array(to_jsonb(retained),to_jsonb(ended))::text FROM access.grants retained JOIN access.terminations ended ON ended.id=retained.id WHERE retained.id=$1", &[&ended.id()]).await.expect("original self source").get(0);
                        let original = operation();
                        if case == "account write" {
                            owner.client().batch_execute("ALTER TABLE identity.accounts ADD CONSTRAINT test_after_grant_projection CHECK (cancellation_id IS NULL)").await.expect("fail Account after Holder and Grant projection writes");
                        } else {
                            proxy.arm(if case == "lost commit" { Fault::LostCommit } else { Fault::UnknownCommit });
                        }
                        let service = services.cancellation().clone();
                        let accepted_actor = if managed { manager.clone() } else { holder.clone() };
                        let account_id = account.id();
                        let self_input = confirmation(&account);
                        let managed_input = management(&account);
                        let task = tokio::spawn(async move {
                            if managed { service.cancel_account(accepted_actor, original, account_id, managed_input).await }
                            else { service.cancel(accepted_actor, original, self_input).await }
                        });
                        if case != "account write" {
                            timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("actual cancellation COMMIT intercepted");
                        }
                        let result = timeout(Duration::from_secs(15), task).await.expect("bounded cancellation outcome").expect("accepted cancellation");
                        if case == "unknown commit" {
                            assert!(matches!(result, Err(crate::cancellation::Error::Unknown(_))), "{:?}", result.as_ref().err());
                            assert_eq!(observer.client().query_one(GRAPH, &[]).await.expect("no effects visible before original COMMIT").get::<_, String>(0), graph);
                        } else if case == "account write" {
                            assert!(result.is_err(), "Account failure rejects every projection");
                            assert_eq!(observer.client().query_one(GRAPH, &[]).await.expect("all current states, history and credentials rolled back").get::<_, String>(0), graph);
                            services.current().read(&holder).await.expect("Account and Session anchors restored");
                            services.grant().read_latest(&holder).await.expect("Grant anchor restored");
                            owner.client().batch_execute("ALTER TABLE identity.accounts DROP CONSTRAINT test_after_grant_projection").await.expect("remove test fault");
                        } else {
                            assert!(result.is_ok(), "{:?}", result.as_ref().err());
                        }
                        proxy.release();
                        transaction::begin(&context, observer.client(), b"identity/cancellation/operation", original).await.expect("original cancellation transaction ended");
                        observer.client().batch_execute("COMMIT").await.expect("release outcome observation lock");
                        if case == "account write" {
                            if managed {
                                Box::pin(services.cancellation().cancel_account(manager.clone(), original, account.id(), management(&account))).await.expect("retry after confirmed complete rollback");
                            } else {
                                Box::pin(services.cancellation().cancel(holder.clone(), original, confirmation(&account))).await.expect("retry after confirmed complete rollback");
                            }
                        }
                        let cause = observer.client().query_one("SELECT id,cancelled_at FROM identity.cancellations WHERE operation_id=$1", &[&original]).await.expect("permanent original result before any replay");
                        assert_cancelled_account(observer.client(), &account, &before, cause.get("id"), cause.get("cancelled_at")).await;
                        let paired = observer.client().query_one("SELECT retained.sequence=2 AND retained.version=2 AND retained.termination='cancellation' AND retained.changed_at=cause.cancelled_at AND ending.sequence=2 AND ending.mode='cancellation' AND ending.cancellation_id=cause.id AND ending.operator_id=cause.operator_id AND ending.revision_id=cause.revision_id AND ending.operation_id=cause.operation_id AND ending.terminated_at=cause.cancelled_at FROM access.grants retained JOIN access.terminations ending ON ending.id=retained.id JOIN identity.cancellations cause ON cause.id=ending.cancellation_id WHERE retained.id=$1 AND cause.operation_id=$2", &[&current.id(), &original]).await.expect("exact Grant source projection").get::<_, bool>(0);
                        assert!(paired);
                        assert_eq!(observer.client().query_one("SELECT jsonb_build_array(to_jsonb(retained),to_jsonb(ended))::text FROM access.grants retained JOIN access.terminations ended ON ended.id=retained.id WHERE retained.id=$1", &[&ended.id()]).await.expect("prior self source remains intact").get::<_, String>(0), old);
                        assert_eq!(observer.client().query_one("SELECT count(*) FROM access.terminations WHERE operation_id=$1", &[&original]).await.expect("only the original unfinished Grant ended").get::<_, i64>(0), 1);
                        if managed {
                            Box::pin(services.cancellation().cancel_account(manager, original, account.id(), management(&account))).await.expect("management replay returns fixed original");
                        } else {
                            assert!(matches!(Box::pin(services.cancellation().cancel(holder, original, confirmation(&account))).await, Err(crate::cancellation::Error::Unauthenticated)));
                        }
                        services.close();
                        services.drain().await.expect("drained");
                        context.stop().await.expect("context stopped");
                        proxy.stop().await;
                        let (context, services) = start(database.connections(&database.user), &supplier).await;
                        services.ready().await.expect("all original projections survive startup");
                        services.close();
                        services.drain().await.expect("restart drained");
                        context.stop().await.expect("restart stopped");
                        observer.terminate().await.expect("observer stopped");
                        owner.terminate().await.expect("owner stopped");
                        assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                        supplier.stop().await;
                    })).await.unwrap_or_else(|error| panic!("managed={managed}, {case}: {error}"));
                }
            }
        }));
    }

    #[test]
    fn role_loss_settles_every_original_grant_across_commit_and_partial_write_failures() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["normal", "lost commit", "unknown commit", "next role", "second projection"] {
                timeout(Duration::from_secs(70), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = register(&database, &services, &mut supplier).await;
                    let unrelated = Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                    let definition = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Admission holders","level":200,"permissions":["workforce.admission"]})).expect("definition"))).await.expect("real custom role");
                    let first = Box::pin(register_numbered(&database, &services, &mut supplier, 101)).await;
                    let second = Box::pin(register_numbered(&database, &services, &mut supplier, 102)).await;
                    let ended_actor = Box::pin(register_numbered(&database, &services, &mut supplier, 103)).await;
                    let first_grant = Box::pin(admit(&services, &institution, Entry::Institution, "13800000101", Kind::Custom)).await;
                    let second_grant = Box::pin(admit(&services, &institution, Entry::Institution, "13800000102", Kind::Custom)).await;
                    let ended = Box::pin(admit(&services, &institution, Entry::Institution, "13800000103", Kind::Custom)).await;
                    let ended_account = services.current().read(&ended_actor).await.expect("holder before cancellation");
                    Box::pin(services.cancellation().cancel_account(institution.clone(), operation(), ended_account.id(), serde_json::from_value(json!({"personId":ended_account.person_id(),"revisionId":ended_account.revision_id(),"mobile":"13800000103"})).expect("ended holder confirmation"))).await.expect("cancellation before the role change");
                    let target = services.current().read(&second).await.expect("second identity");
                    let pause = Box::pin(services.grant().suspend(manager, operation(), second_grant.id(), serde_json::from_value(json!({"target":{"accountId":target.id(),"personId":target.person_id(),"revisionId":target.revision_id(),"mobile":"13800000102"}})).expect("pause target"))).await.expect("paused current holder");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                    let before: String = observer.client().query_one(FACTS, &[]).await.expect("complete original graph").get(0);
                    let controls: String = observer.client().query_one("SELECT jsonb_agg(value ORDER BY id)::text FROM access.grants value WHERE id=$1 OR id=$2", &[&ended.id(), &unrelated.id()]).await.expect("unchanged controls").get(0);
                    let original = operation();
                    let body = json!({"entry":"institution","changeId":definition.change_id(),"name":"Admission removed","level":200,"permissions":[]});
                    if case == "second projection" {
                        let rejected = if first_grant.id().as_bytes() > second_grant.id().as_bytes() { first_grant.id() } else { second_grant.id() };
                        owner.client().batch_execute(&format!("ALTER TABLE access.losses ADD CONSTRAINT test_second_projection CHECK (grant_id <> '{}'::uuid)", Uuid::from_bytes(*rejected.as_bytes()))).await.expect("fail the second sorted Grant write");
                    } else if case == "lost commit" {
                        proxy.arm(Fault::LostCommit);
                    } else if matches!(case, "unknown commit" | "next role") {
                        proxy.arm(Fault::UnknownCommit);
                    }
                    let service = services.role().clone();
                    let accepted_actor = institution.clone();
                    let role_id = definition.id();
                    let input = serde_json::from_value(body.clone()).expect("fixed original request");
                    let task = tokio::spawn(async move { service.update_workforce(accepted_actor, original, role_id, input).await });
                    if matches!(case, "lost commit" | "unknown commit" | "next role") {
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("intercept actual original COMMIT");
                    }
                    let result = timeout(Duration::from_secs(15), task).await.expect("bounded original outcome").expect("accepted task");
                    if matches!(case, "unknown commit" | "next role") {
                        assert!(matches!(result, Err(kernel::Error::Unknown(_))), "{case}: {:?}", result.as_ref().err());
                        assert_eq!(observer.client().query_one(FACTS, &[]).await.expect("original is still uncommitted").get::<_, String>(0), before);
                    } else if case == "second projection" {
                        assert!(result.is_err(), "the second projection failure rejects the entire operation");
                        assert_eq!(observer.client().query_one(FACTS, &[]).await.expect("no partial role, Grant, loss or completion").get::<_, String>(0), before);
                        services.grant().read_latest(&first).await.expect("first tentative anchor rolled back");
                        services.grant().read_latest(&second).await.expect("second tentative anchor rolled back");
                        owner.client().batch_execute("ALTER TABLE access.losses DROP CONSTRAINT test_second_projection").await.expect("remove only this test fault");
                    } else {
                        assert!(result.is_ok(), "{case}: {:?}", result.as_ref().err());
                    }
                    proxy.release();
                    transaction::begin(&context, observer.client(), Family::WorkforceRole.lock(), original).await.expect("original backend released its operation lock");
                    observer.client().batch_execute("COMMIT").await.expect("observer released original lock");
                    if case == "second projection" {
                        Box::pin(services.role().update_workforce(institution.clone(), original, definition.id(), serde_json::from_value(body.clone()).expect("unchanged retry"))).await.expect("confirmed rollback permits a new original attempt");
                    }
                    let cause: role::change::Id = observer.client().query_one("SELECT id FROM access.changes WHERE operation_id=$1", &[&original]).await.expect("one permanent role result before replay").get(0);
                    for (id, sequence, suspension) in [(first_grant.id(), 2_i64, None), (second_grant.id(), 3_i64, Some(pause.id()))] {
                        let row = observer.client().query_one("SELECT retained.sequence,retained.version,retained.epoch,retained.suspension_id,(SELECT count(*) FROM access.losses loss WHERE loss.grant_id=retained.id AND loss.change_id=$2 AND loss.operation_id=$3 AND loss.sequence=retained.sequence AND loss.epoch=retained.epoch AND loss.projected_at=retained.changed_at) AS projections FROM access.grants retained WHERE retained.id=$1", &[&id, &cause, &original]).await.expect("complete original projection before replay");
                        assert_eq!(row.get::<_, i64>("sequence"), sequence);
                        assert_eq!(row.get::<_, i64>("version"), sequence);
                        assert_eq!(row.get::<_, i64>("epoch"), 1);
                        assert_eq!(row.get::<_, Option<grant::suspension::Id>>("suspension_id"), suspension);
                        assert_eq!(row.get::<_, i64>("projections"), 1);
                    }
                    assert_eq!(observer.client().query_one("SELECT jsonb_agg(value ORDER BY id)::text FROM access.grants value WHERE id=$1 OR id=$2", &[&ended.id(), &unrelated.id()]).await.expect("ended and unrelated states preserved").get::<_, String>(0), controls);
                    services.grant().read_latest(&first).await.expect("the committed first projection remains readable");
                    services.grant().read_latest(&second).await.expect("the committed paused projection remains readable");
                    if case == "next role" {
                        let restored = Box::pin(services.role().update_workforce(institution.clone(), operation(), definition.id(), serde_json::from_value(json!({"entry":"institution","changeId":cause,"name":"Restored admission","level":200,"permissions":["workforce.admission"]})).expect("restoration input"))).await.expect("the next role writer settles every original Grant anchor");
                        let removed = Box::pin(services.role().update_workforce(institution.clone(), operation(), definition.id(), serde_json::from_value(json!({"entry":"institution","changeId":restored.change_id(),"name":"Admission lost again","level":200,"permissions":[]})).expect("second loss input"))).await.expect("a later role loss appends exactly one further projection");
                        assert!(observer.client().query_one("SELECT count(*)=2 AND bool_and(state.epoch=2 AND state.version=state.sequence AND loss.sequence=state.sequence) FROM access.grants state JOIN access.losses loss ON loss.grant_id=state.id WHERE loss.change_id=$1", &[&removed.change_id()]).await.expect("both original Grant states advance once more").get::<_, bool>(0));
                    }
                    let candidate = services.assignment().candidate(&institution, Entry::Institution, "13800000101".parse().expect("mobile"), Pagination::new(1,20).expect("page")).await.expect("later reassignment");
                    let selected = candidate.roles().iter().find(|role| role.kind()==Kind::Default).expect("default role");
                    let subject = candidate.identity();
                    let replacement = json!({"entry":"institution","target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":"13800000101","grantId":candidate.grant_id(),"assignmentId":candidate.assignment_id()},"role":{"id":selected.id(),"changeId":selected.change_id()}});
                    Box::pin(services.assignment().assign(institution.clone(), operation(), serde_json::from_value(replacement).expect("replacement"))).await.expect("original holder moved away");
                    Box::pin(services.cancellation().cancel_account(institution.clone(), operation(), target.id(), serde_json::from_value(json!({"personId":target.person_id(),"revisionId":target.revision_id(),"mobile":"13800000102"})).expect("paused holder confirmation"))).await.expect("cancellation after the role loss");
                    let extra = i64::from(case == "next role");
                    assert!(observer.client().query_one("SELECT state.epoch=1+$3::bigint AND state.sequence=4+$3::bigint AND state.version=state.sequence AND ending.sequence=state.sequence AND ending.mode='cancellation' AND loss.sequence=3 FROM access.grants state JOIN access.terminations ending ON ending.id=state.id JOIN access.losses loss ON loss.grant_id=state.id WHERE state.id=$1 AND loss.operation_id=$2", &[&second_grant.id(), &original, &extra]).await.expect("loss precedes the cancellation projection").get::<_, bool>(0));
                    let after: String = observer.client().query_one(FACTS, &[]).await.expect("facts before original replay").get(0);
                    let replay = Box::pin(services.role().update_workforce(institution, original, definition.id(), serde_json::from_value(body).expect("original request"))).await.expect("recovery uses original losses rather than current holders");
                    assert_eq!(replay.change_id(), cause);
                    assert_eq!(observer.client().query_one(FACTS, &[]).await.expect("replay writes no projection").get::<_, String>(0), after);
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("context stopped");
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    services.ready().await.expect("all retained role and Grant histories authenticate");
                    services.close();
                    services.drain().await.expect("restart drained");
                    context.stop().await.expect("restart stopped");
                    observer.terminate().await.expect("observer stopped");
                    owner.terminate().await.expect("owner stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    supplier.stop().await;
                    proxy.stop().await;
                })).await.unwrap_or_else(|error| panic!("{case}: {error}"));
            }
        }));
    }

    #[test]
    fn startup_authenticates_grant_records_across_batch_boundaries() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = register(&database, &services, &mut supplier).await;
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Full)).await;
                let target = Box::pin(admit(&services, &manager, Entry::Domain, "13900000000", Kind::Default)).await;
                let account = services.current().read(&institution).await.expect("target identity");
                let body = json!({"target":{"accountId":account.id(),"personId":account.person_id(),"revisionId":account.revision_id(),"mobile":"13900000000"}});
                for _ in 0..65 {
                    let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), target.id(), serde_json::from_value(body.clone()).expect("pause input"))).await.expect("next pause");
                    Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(body.clone()).expect("resume input"))).await.expect("matching resume");
                }
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                assert_eq!(observer.client().query_one("SELECT sequence FROM access.grants WHERE id=$1", &[&target.id()]).await.expect("more than one record batch").get::<_, i64>(0), 131);
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.ready().await.expect("all record and completion batches authenticated");
                assert_eq!(services.grant().read_latest(&institution).await.expect("restored anchor").id(), target.id());
                services.close();
                services.drain().await.expect("restart drained");
                context.stop().await.expect("restart stopped");
                observer.terminate().await.expect("observer stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
            })).await.expect("bounded multi-batch startup");
        }));
    }
}
