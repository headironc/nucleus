//! Workforce role assignments, preserved independently of current membership.

pub(super) mod database;
pub(super) mod operation;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, timeout_at};
use tokio_postgres::{
    Client,
    types::{FromSql, ToSql},
};
use utoipa::ToSchema;
use uuid::Uuid;

pub use crate::access::assignment::operation::{CommandError, Input, Rejection, Target};
use crate::{
    access::{Entry, appointment, capability::Scope, grant, role},
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
#[schema(as = assignment::Id, value_type = Uuid)]
pub struct Id(Uuid);

impl Id {
    pub(in crate::access) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }

    pub(in crate::access) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

pub struct Assignment {
    id: Id,
    grant_id: grant::Id,
    subject: revision::Identity,
    operator: revision::Identity,
    role: role::Role,
    sequence: i64,
    assigned_at: DateTime<Utc>,
    operation_id: identity::operation::Id,
}

impl Assignment {
    pub(in crate::access) fn first(id: Id, grant: &grant::Grant, role: role::Role) -> Self {
        Self {
            id,
            grant_id: grant.id(),
            subject: grant.subject().clone(),
            operator: grant.operator().clone(),
            role,
            sequence: 1,
            assigned_at: grant.granted_at(),
            operation_id: grant.operation_id(),
        }
    }
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn grant_id(&self) -> grant::Id {
        self.grant_id
    }
    pub fn subject(&self) -> &revision::Identity {
        &self.subject
    }
    pub fn operator(&self) -> &revision::Identity {
        &self.operator
    }
    pub fn role(&self) -> &role::Role {
        &self.role
    }
    pub fn sequence(&self) -> i64 {
        self.sequence
    }
    pub fn assigned_at(&self) -> DateTime<Utc> {
        self.assigned_at
    }
    pub fn operation_id(&self) -> identity::operation::Id {
        self.operation_id
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Filter {
    #[serde(flatten)]
    pagination: Pagination,
    person_id: Option<person::Id>,
    operation_id: Option<identity::operation::Id>,
}

pub struct List {
    count: i64,
    assignments: Vec<Assignment>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }
}

/// The first Grant record fixes its selected definition independently of later changes.
pub(super) async fn fetch_first(
    client: &Client,
    roles: &role::Reader,
    grant_id: grant::Id,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<Assignment, ReadError> {
    use crate::history::{self, Kind as _, Record as _};
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.grant_id=$1 AND record.sequence=1",
                grant::record::Record::SELECT
            ),
            &[&grant_id],
        )
        .await?
        .ok_or(history::Error::Integrity(
            "a Grant retains its first assignment",
        ))?;
    let record = history::record::<grant::record::Record>(&row, roles.keys(), now)?;
    let grant::record::Event::Assignment {
        change_id,
        operator_id,
        revision_id,
        ..
    } = record.event()
    else {
        return Err(history::Error::Integrity("a Grant begins with an assignment").into());
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
    .ok_or(history::Error::Integrity(
        "a first assignment retains its selected definition",
    ))?;
    let subject = revisions.fetch_identity(client, *revision_id).await?;
    if subject.person_id() != record.person_id() {
        return Err(history::Error::Integrity("a first assignment retains its subject").into());
    }
    Ok(Assignment {
        id: record.assignment_id(),
        grant_id,
        subject,
        operator: revisions.fetch_identity(client, *operator_id).await?,
        role,
        sequence: record.sequence(),
        assigned_at: record.happened_at(),
        operation_id: record.operation_id(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("domain assignment query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("{0}")]
    Role(#[from] role::Error),
    #[error("{0}")]
    Revision(#[from] revision::ReadError),
    #[error("{0}")]
    History(#[from] crate::history::Error),
}

impl From<tokio_postgres::Error> for ReadError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(error.into())
    }
}

impl ReadError {
    pub(super) fn unreliable(&self) -> bool {
        match self {
            Self::Role(error) => crate::runtime::work::Failure::unreliable(error),
            Self::Revision(error) => error.unreliable(),
            Self::Query(_) => false,
            Self::History(error) => error.corrupt(),
        }
    }
}

pub struct Candidate {
    identity: account::Subject,
    grant_id: grant::Id,
    assignment_id: Id,
    role: role::Role,
    roles: role::List,
}

impl Candidate {
    pub fn identity(&self) -> &account::Subject {
        &self.identity
    }
    pub fn grant_id(&self) -> grant::Id {
        self.grant_id
    }
    pub fn assignment_id(&self) -> Id {
        self.assignment_id
    }
    pub fn role(&self) -> &role::Role {
        &self.role
    }
    pub fn count(&self) -> i64 {
        self.roles.count()
    }
    pub fn roles(&self) -> &[role::Role] {
        self.roles.roles()
    }
}

#[derive(Clone)]
pub struct Service {
    app: App,
    context: Context,
    roles: role::Reader,
    current: current::Service,
    accounts: account::Reader,
    revisions: revision::Reader,
    registry: Arc<Registry>,
}

impl Service {
    pub(crate) fn prepare(app: App, current: current::Service, keys: Arc<keys::Keys>) -> Self {
        Self {
            app: app.clone(),
            context: app.context().clone(),
            registry: Arc::clone(app.registry()),
            roles: role::Reader::new(app.clone()),
            current,
            accounts: account::Reader::new(app.clone(), keys),
            revisions: revision::Reader::new(app),
        }
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
                        let (_, _, _, now) = grant::member::authorize(
                            client,
                            &roles,
                            &self.current,
                            actor,
                            role::Code::WorkforceAssignments,
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
                        let (manager, person_id, _, now) =
                            self.authorize(client, &roles, actor, entry).await?;
                        let account_id = self
                            .accounts
                            .at(view)
                            .find_by_mobile(client, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        let member = Self::fetch_target(
                            client, &roles, entry, &manager, person_id, account_id, now,
                        )
                        .await?;
                        let identity = self
                            .accounts
                            .at(view)
                            .find_by_id(client, account_id, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
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
                                    &manager,
                                    now,
                                )
                                .await?
                            }
                        };
                        Ok(Candidate {
                            identity,
                            grant_id: member.id(),
                            assignment_id: member.assignment_id(),
                            role: member.into_role(),
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

    async fn fetch_target(
        client: &Client,
        roles: &role::Reader,
        entry: Entry,
        manager: &role::Role,
        operator_id: person::Id,
        account_id: account::Id,
        now: DateTime<Utc>,
    ) -> Result<grant::member::Member, Error> {
        let member = grant::member::find_by_account(client, roles, account_id, now)
            .await?
            .ok_or(Error::Absent)?;
        if member.person_id() == operator_id {
            return Err(Error::Absent);
        }
        let level = match entry {
            Entry::Domain => member.role().level(),
            Entry::Institution => appointment::find_by_account(client, roles, account_id, now)
                .await?
                .ok_or(Error::Absent)?
                .role()
                .level(),
        };
        if level >= manager.level() {
            return Err(Error::Absent);
        }
        Ok(member)
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
    #[error("role assignment is denied")]
    Denied,
    #[error("assignment input differs from the original operation")]
    Mismatch,
    #[error("the target is unavailable")]
    Absent,
    #[error("the confirmed target has changed")]
    TargetChanged,
    #[error("the selected role has changed")]
    RoleChanged,
    #[error("the member already holds the selected role")]
    Unchanged,
    #[error("{0}")]
    Current(#[from] current::Error),
    #[error("{0}")]
    Role(#[from] role::Error),
    #[error("{0}")]
    Appointment(#[from] appointment::Error),
    #[error("{0}")]
    Membership(#[source] Box<grant::Error>),
    #[error("{0}")]
    Read(#[from] ReadError),
    #[error("{0}")]
    Account(#[from] account::ReadError),
    #[error("assignment fact check failed: {rule}")]
    Facts { rule: &'static str },
    #[error("assignment query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("assignment decoding failed: {0}")]
    Decode(#[source] crate::database::Query),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("{0}")]
    Work(#[from] runtime::work::Error),
    #[error("assignment input or coordination encoding failed")]
    Crypto(#[from] platform::cryptography::Error),
    #[error("{0}")]
    Key(#[from] keys::request::Missing),
    #[error("assignment transaction end failed: {0}")]
    End(#[source] transaction::EndError),
    #[error("original assignment did not commit")]
    NotCommitted {
        #[source]
        cause: Box<Error>,
        commit: Result<(), transaction::EndError>,
        termination: Result<(), platform::database::Error>,
    },
    #[error("original assignment outcome remains unknown")]
    Unknown {
        #[source]
        cause: Box<Error>,
        recovery: Box<Error>,
    },
    #[error("assignment budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("assignment rejection rollback failed")]
    Rollback {
        #[source]
        cause: Box<Error>,
        rollback: transaction::EndError,
    },
    #[error("assignment and connection termination failed")]
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

impl From<grant::Error> for Error {
    fn from(error: grant::Error) -> Self {
        match error {
            grant::Error::Denied => Self::Denied,
            grant::Error::Current(error) => Self::Current(error),
            grant::Error::Role(error) => Self::Role(error),
            grant::Error::Assignment(error) => Self::Read(error),
            error => Self::Membership(Box::new(error)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum Failure {
    Invalid,
    Denied,
    Mismatch,
    Absent,
    TargetChanged,
    RoleChanged,
    Unchanged,
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
                | Self::TargetChanged
                | Self::RoleChanged
                | Self::Unchanged
                | Self::Current(current::Error::Invalid)
                | Self::Role(role::Error::Denied | role::Error::Current(current::Error::Invalid))
        )
    }
    pub fn failure(&self) -> Failure {
        match self {
            Self::Denied | Self::Role(role::Error::Denied) => Failure::Denied,
            Self::Mismatch => Failure::Mismatch,
            Self::Absent => Failure::Absent,
            Self::TargetChanged => Failure::TargetChanged,
            Self::RoleChanged => Failure::RoleChanged,
            Self::Unchanged => Failure::Unchanged,
            Self::Current(current::Error::Invalid)
            | Self::Role(role::Error::Current(current::Error::Invalid)) => Failure::Invalid,
            Self::Work(runtime::work::Error::Busy) => Failure::Busy,
            Self::Work(runtime::work::Error::Worker) => Failure::Worker,
            Self::Unknown { .. } | Self::Command(crate::kernel::Error::Unknown(_)) => {
                Failure::Unknown
            }
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
            Self::Facts { .. } | Self::Decode(_) | Self::Key(_) => true,
            Self::Command(error) => error.is_corrupt(),
            Self::Current(error) => error.unreliable(),
            Self::Role(error) => error.unreliable(),
            Self::Appointment(error) => error.unreliable(),
            Self::Membership(error) => error.unreliable(),
            Self::Read(error) => error.unreliable(),
            Self::Account(error) => error.unreliable(),
            Self::Combined { cause, .. }
            | Self::Rollback { cause, .. }
            | Self::NotCommitted { cause, .. } => cause.unreliable(),
            Self::Unknown { cause, recovery } => cause.unreliable() || recovery.unreliable(),
            _ => false,
        }
    }
    fn report(&self, operation_id: identity::operation::Id) {
        if !self.rejected() {
            tracing::error!(%operation_id, stage = "assignment_completion", kind = self.failure().as_ref(), error = ?self);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use serde_json::{Value, json};
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::{
            Entry,
            appointment::tests::register,
            assignment::{Candidate, Error, Filter, Input},
            role::{
                self,
                tests::{actor, start},
            },
        },
        identity::{
            authentication::{Database, Supplier, server},
            operation,
        },
        pagination::Pagination,
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }
    fn pagination() -> Pagination {
        Pagination::new(1, 100).expect("page")
    }
    fn filter() -> Filter {
        Filter {
            pagination: pagination(),
            person_id: None,
            operation_id: None,
        }
    }
    fn command(value: &Value) -> Input {
        serde_json::from_value(value.clone()).expect("typed assignment")
    }
    fn input(candidate: &Candidate, selected: &role::Role, entry: Entry, mobile: &str) -> Value {
        let subject = candidate.identity();
        json!({"entry":entry.as_ref(),"target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":mobile,"grantId":candidate.grant_id(),"assignmentId":candidate.assignment_id()},"role":{"id":selected.id(),"changeId":selected.change_id()}})
    }

    async fn admit(
        services: &crate::startup::Services,
        actor: &crate::identity::current::Actor,
        entry: Entry,
        mobile: &str,
        kind: role::Kind,
    ) -> (
        crate::access::grant::Grant,
        crate::access::assignment::Assignment,
    ) {
        let candidate = services
            .grant()
            .candidate(actor, entry, mobile.parse().expect("mobile"), pagination())
            .await
            .expect("admission candidate");
        let selected = candidate
            .roles()
            .iter()
            .find(|role| role.kind() == kind)
            .expect("selected role");
        let subject = candidate.identity();
        let value = json!({"entry":entry.as_ref(),"target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":mobile,"assignmentId":candidate.assignment_id(),"suspensionId":candidate.suspension_id(),"grant":candidate.grant()},"role":{"id":selected.id(),"changeId":selected.change_id()}});
        Box::pin(services.grant().create(
            actor.clone(),
            operation(),
            serde_json::from_value(value).expect("admission"),
        ))
        .await
        .expect("real admission")
    }

    #[test]
    fn history_retains_initial_and_replaced_roles_across_ended_and_cancelled_grants() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                for actor in [&institution, &member] {
                    assert!(matches!(services.assignment().list(actor, filter()).await, Err(Error::Denied)), "institutional authority or an ordinary account does not grant history access");
                }
                let (grant, first) = Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await;
                let (_, own_first) = Box::pin(admit(&services, &member, Entry::Domain, "13900000000", role::Kind::Default)).await;
                let reader = Box::pin(services.role().create_workforce(member.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"History reader","level":200,"permissions":["workforce.assignments"]})).expect("reader definition"))).await.expect("domain reader role");
                let candidate = services.assignment().candidate(&member, Entry::Domain, "13900000000".parse().expect("mobile"), pagination()).await.expect("reader confirmation");
                let body = input(&candidate, &reader, Entry::Domain, "13900000000");
                let assigned = Box::pin(services.assignment().assign(member.clone(), first.operation_id(), command(&body))).await.expect("replacement reuses a Grant family's operation UUID");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let facts = observer.client().query("SELECT assignment.id, assignment.grant_id, assignment.sequence, assignment.assigned_at, assignment.operation_id, subject.person_id, subject.id AS revision_id, operator.person_id AS operator_person, operator.id AS operator_id FROM access.assignments assignment JOIN identity.revisions subject ON subject.id=assignment.revision_id JOIN identity.revisions operator ON operator.id=assignment.operator_id ORDER BY assignment.assigned_at DESC, assignment.id DESC", &[]).await.expect("permanent facts before any history read");
                assert_eq!(facts.len(),3);
                assert_eq!(facts.iter().map(|row|row.get::<_,super::Id>("id")).collect::<Vec<_>>(),[assigned.id(),own_first.id(),first.id()]);
                let list = services.assignment().list(&institution, filter()).await.expect("lower reader sees self and full-rank member");
                assert_eq!(list.count(),3);
                for (assignment,row) in list.assignments().iter().zip(&facts) {
                    assert_eq!(assignment.id(),row.get("id"));
                    assert_eq!(assignment.grant_id(),row.get("grant_id"));
                    assert_eq!(assignment.sequence(),row.get::<_,i64>("sequence"));
                    assert_eq!(assignment.assigned_at(),row.get::<_,chrono::DateTime<chrono::Utc>>("assigned_at"));
                    assert_eq!(assignment.operation_id(),row.get("operation_id"));
                    assert_eq!(assignment.subject().person_id(),row.get("person_id"));
                    assert_eq!(assignment.subject().revision_id(),row.get("revision_id"));
                    assert_eq!(assignment.operator().person_id(),row.get("operator_person"));
                    assert_eq!(assignment.operator().revision_id(),row.get("operator_id"));
                }
                assert_eq!(list.assignments()[0].role().name(),reader.name());
                assert_eq!(list.assignments()[1].role().kind(),role::Kind::Default);
                assert_eq!(list.assignments()[2].role().kind(),role::Kind::Full);
                for (page,size,person_id,operation_id,count,ids) in [
                    (1,1,None,None,3,vec![assigned.id()]),
                    (2,1,None,None,3,vec![own_first.id()]),
                    (i64::MAX,1,None,None,3,vec![]),
                    (1,100,Some(candidate.identity().person_id()),None,2,vec![assigned.id(),own_first.id()]),
                    (1,100,None,Some(first.operation_id()),2,vec![assigned.id(),first.id()]),
                    (1,100,Some(first.subject().person_id()),Some(first.operation_id()),1,vec![first.id()]),
                    (1,100,Some(crate::identity::person::Id::allocate()),None,0,vec![]),
                    (1,100,None,Some(operation()),0,vec![]),
                ] {
                    let list = services.assignment().list(&institution, Filter { pagination: Pagination::new(page,size).expect("page"), person_id, operation_id }).await.expect("filtered permanent page");
                    assert_eq!(list.count(),count); assert_eq!(list.assignments().iter().map(super::Assignment::id).collect::<Vec<_>>(),ids);
                }
                Box::pin(services.role().update_workforce(member.clone(),operation(),reader.id(),serde_json::from_value(json!({"entry":"domain","changeId":reader.change_id(),"name":"Renamed reader","level":200,"permissions":["workforce.assignments"]})).expect("reader update"))).await.expect("current definition changes independently of assignment time");
                let replay = Box::pin(services.assignment().assign(member.clone(),first.operation_id(),command(&body))).await.expect("original recovery still selects only the replacement family");
                assert_eq!(replay.id(),assigned.id()); assert_eq!(replay.role().name(),reader.name());
                let temporary = Box::pin(services.role().create_workforce(institution.clone(),operation(),serde_json::from_value(json!({"entry":"institution","name":"Historical role","level":700,"permissions":[]})).expect("temporary definition"))).await.expect("temporary role");
                let target = services.assignment().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),pagination()).await.expect("full member target");
                let replaced = Box::pin(services.assignment().assign(institution.clone(),operation(),command(&input(&target,&temporary,Entry::Institution,"13800000000")))).await.expect("temporary assignment");
                let changed = Box::pin(services.role().update_workforce(institution.clone(),operation(),temporary.id(),serde_json::from_value(json!({"entry":"institution","changeId":temporary.change_id(),"name":"Changed later","level":701,"permissions":[]})).expect("later definition"))).await.expect("later role modification");
                let target = services.assignment().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),pagination()).await.expect("restoration confirmation");
                Box::pin(services.assignment().assign(institution.clone(),operation(),command(&input(&target,first.role(),Entry::Institution,"13800000000")))).await.expect("restore full role");
                Box::pin(services.role().delete_workforce(institution.clone(),operation(),temporary.id(),serde_json::from_value(json!({"entry":"institution","changeId":changed.change_id()})).expect("deletion"))).await.expect("unused current definition removed");
                Box::pin(services.correction().correct(institution.clone(),operation(),serde_json::from_value(json!({"target":{"personId":first.subject().person_id(),"revisionId":first.subject().revision_id()},"name":"Changed Person","idNumber":"00000020000229007X"})).expect("identity correction"))).await.expect("actual later identity");
                let revised = services.current().read(&member).await.expect("new fixed revision");
                Box::pin(services.grant().terminate(member.clone(),operation(),grant.id())).await.expect("actual self exit");
                let (rejoined,new_first) = Box::pin(admit(&services,&institution,Entry::Institution,"13800000000",role::Kind::Full)).await;
                Box::pin(services.cancellation().cancel_account(institution.clone(),operation(),revised.id(),serde_json::from_value(json!({"personId":revised.person_id(),"revisionId":revised.revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("actual cancellation after rejoining");
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.assignments)=6 AND (SELECT count(*) FROM access.grants WHERE person_id=$1)=2 AND EXISTS(SELECT 1 FROM access.terminations WHERE id=$2) AND EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$1) AND NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND person_id=$1) AND EXISTS(SELECT 1 FROM access.roles state JOIN access.changes record ON record.id=state.change_id AND record.role_id=state.id AND record.sequence=state.sequence AND record.changed_at=state.changed_at WHERE state.id=$3 AND NOT state.active AND state.sequence=3 AND state.version=3 AND cardinality(state.permissions)=0 AND record.action='delete' AND record.permissions IS NULL)",&[&revised.person_id(),&grant.id(),&temporary.id()]).await.expect("retained facts and removed current relationships before reading").get::<_,bool>(0));
                let list = services.assignment().list(&institution,filter()).await.expect("all permanent histories survive");
                assert_eq!(list.count(),6); assert_eq!(list.assignments().len(),6);
                let old = list.assignments().iter().find(|value|value.id()==replaced.id()).expect("deleted role remains explainable");
                assert_eq!(old.role().name(),temporary.name()); assert_eq!(old.role().change_id(),temporary.change_id());
                assert_eq!(old.subject().revision_id(),first.subject().revision_id()); assert_eq!(old.subject().name(),"T*");
                let original = list.assignments().iter().find(|value|value.id()==assigned.id()).expect("fixed earlier operator");
                assert_eq!(original.operator().revision_id(),first.subject().revision_id()); assert_eq!(original.operator().name(),"T*");
                assert_eq!(original.role().name(),reader.name());
                let latest = list.assignments().iter().find(|value|value.id()==new_first.id()).expect("cancelled latest Grant's first assignment");
                assert_eq!(latest.grant_id(),rejoined.id()); assert_eq!(latest.subject().revision_id(),revised.revision_id()); assert_eq!(latest.subject().name(),"C*");
                let personal = services.assignment().list(&institution,Filter { person_id:Some(revised.person_id()), ..filter() }).await.expect("history includes every Grant of the original person");
                assert_eq!(personal.count(),4);
                assert!(personal.assignments().windows(2).all(|pair|(pair[0].assigned_at(),pair[0].id().0)>(pair[1].assigned_at(),pair[1].id().0)));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.assignments",&[]).await.expect("read preserves facts").get::<_,i64>(0),6);
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err(),"monitor includes all history, replay and drain"); supplier.stop().await;
            })).await.expect("bounded permanent assignment history");
        }));
    }

    #[test]
    fn history_requires_current_domain_permission_and_unsuspended_membership() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await;
                let reader = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"History reader","level":200,"permissions":["workforce.roles"]})).expect("role"))).await.expect("separate maintenance permission");
                let (grant, first) = Box::pin(admit(&services, &member, Entry::Domain, "13900000000", role::Kind::Custom)).await;
                assert!(matches!(services.assignment().list(&institution, filter()).await, Err(Error::Denied)), "neither institutional nor domain maintenance authority grants assignment history");
                Box::pin(services.role().update_workforce(member.clone(), operation(), reader.id(), serde_json::from_value(json!({"entry":"domain","changeId":reader.change_id(),"name":"History reader","level":200,"permissions":["workforce.assignments"]})).expect("reader permission"))).await.expect("exact permission");
                assert_eq!(services.assignment().list(&institution, filter()).await.expect("domain reader").count(), 2);
                let current = services.current().read(&institution).await.expect("fixed subject");
                let target = json!({"target":{"accountId":current.id(),"personId":current.person_id(),"revisionId":current.revision_id(),"mobile":"13900000000"}});
                let paused = Box::pin(services.grant().suspend(member.clone(), operation(), grant.id(), serde_json::from_value(target.clone()).expect("pause confirmation"))).await.expect("real domain pause");
                assert!(matches!(services.assignment().list(&institution, filter()).await, Err(Error::Denied)));
                Box::pin(services.grant().resume(member.clone(), operation(), paused.id(), serde_json::from_value(target).expect("resume confirmation"))).await.expect("real domain resumption");
                assert_eq!(services.assignment().list(&institution, filter()).await.expect("resumed reader").count(), 2);
                Box::pin(services.grant().terminate(institution.clone(), operation(), grant.id())).await.expect("real self exit");
                assert!(matches!(services.assignment().list(&institution, filter()).await, Err(Error::Denied)));
                let retained = services.assignment().list(&member, filter()).await.expect("another authorized member retains access");
                assert!(retained.assignments().iter().any(|assignment| assignment.id() == first.id()));
                Box::pin(services.revocation().revoke(member.clone(), operation(), crate::identity::revocation::Scope::Current)).await.expect("actual session revocation");
                assert_eq!(services.assignment().list(&member, filter()).await.err().expect("old actor rejected").failure(), super::Failure::Invalid);
                services.ready().await.expect("business denials preserve admission");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded current history authority");
        }));
    }

    #[test]
    fn history_authority_count_and_fixed_projections_share_one_snapshot() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::StalledRoleCount, Fault::StalledRolePage] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = crate::access::role::tests::start_snapshot(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let member = register(&database, &services, &mut supplier).await;
                    let reader = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Snapshot reader","level":200,"permissions":["workforce.assignments"]})).expect("reader"))).await.expect("reader role");
                    let (_, first) = Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await;
                    let (_, own) = Box::pin(admit(&services, &member, Entry::Domain, "13900000000", role::Kind::Custom)).await;
                    let replacement = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Replacement manager","level":300,"permissions":["workforce.roles","workforce.assignments"]})).expect("replacement definition"))).await.expect("replacement retains authority to update the reader role");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.assignments", &[]).await.expect("initial committed history before concurrent writes").get::<_, i64>(0), 2);
                    proxy.arm(fault);
                    let service = services.assignment().clone(); let actor = institution.clone();
                    let task = tokio::spawn(async move { service.list(&actor, filter()).await });
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual history boundary after authority");
                    Box::pin(services.role().update_workforce(member.clone(), operation(), reader.id(), serde_json::from_value(json!({"entry":"domain","changeId":reader.change_id(),"name":"Permission removed","level":200,"permissions":[]})).expect("changed definition"))).await.expect("concurrent permission loss");
                    let corrected = Box::pin(services.correction().correct(institution.clone(), operation(), serde_json::from_value(json!({"target":{"personId":first.subject().person_id(),"revisionId":first.subject().revision_id()},"name":"Changed Person","idNumber":"00000020000229007X"})).expect("correction"))).await.expect("concurrent new identity");
                    let corrected = corrected.correction().id();
                    let candidate = services.assignment().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), pagination()).await.expect("corrected target confirmation");
                    let added = Box::pin(services.assignment().assign(institution.clone(), operation(), command(&input(&candidate, &replacement, Entry::Institution, "13800000000")))).await.expect("concurrent new history").id();
                    proxy.release();
                    let retained = timeout(Duration::from_secs(3), task).await.expect("bounded snapshot read").expect("reader task").expect("fixed projections remain valid in the authorized snapshot");
                    assert_eq!(retained.count(), 2);
                    assert!(retained.assignments().iter().any(|item| item.id() == own.id()));
                    assert!(retained.assignments().iter().any(|item| item.id() == first.id() && item.subject().revision_id() == first.subject().revision_id()));
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.assignments WHERE id=$1) AND EXISTS(SELECT 1 FROM access.assignments WHERE id=$2)", &[&own.id(), &first.id()]).await.expect("both original facts remain permanent after the snapshot read").get::<_, bool>(0));
                    assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.assignments)=3 AND EXISTS(SELECT 1 FROM access.assignments WHERE id=$1 AND revision_id=$2) AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$3 AND revision_id=$2) AND NOT EXISTS(SELECT 1 FROM access.permissions WHERE role_id=$4)", &[&added,&corrected,&first.subject().person_id(),&reader.id()]).await.expect("independent committed concurrent facts").get::<_, bool>(0));
                    let latest = services.assignment().list(&member, filter()).await.expect("new facts form a valid history in a fresh snapshot");
                    assert_eq!(latest.count(), 3);
                    assert_eq!(latest.assignments()[0].id(), added);
                    assert_eq!(latest.assignments()[0].subject().revision_id(), corrected.revision_id());
                    assert_eq!(latest.assignments()[0].subject().name(), "C*");
                    assert!(matches!(services.assignment().list(&institution, filter()).await, Err(Error::Denied)), "next request sees loss of exact domain permission");
                    proxy.arm(Fault::RejectedRollback);
                    let error = services.assignment().list(&institution, filter()).await.err().expect("failed denial rollback");
                    let cause = match &error { Error::Combined { cause, .. } => cause.as_ref(), _ => &error };
                    assert!(matches!(cause, Error::Rollback { cause, .. } if matches!(cause.as_ref(), Error::Denied)), "{error:?}");
                    services.ready().await.expect("denial rollback transport failure preserves admission"); observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded snapshot scenario");
            }
        }));
    }

    #[test]
    fn history_rejects_damaged_dependencies_and_recovers_ordinary_query_failures() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["grant", "predecessor", "proof", "subject", "operator", "name", "id_number", "definition", "selected_definition", "future", "query"] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let reader = register(&database, &services, &mut supplier).await;
                    Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await;
                    let (grant, first) = Box::pin(admit(&services, &reader, Entry::Domain, "13900000000", role::Kind::Default)).await;
                    let selected = Box::pin(services.role().create_workforce(reader.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"Historical definition","level":200,"permissions":[]})).expect("definition"))).await.expect("temporary role");
                    let candidate = services.assignment().candidate(&reader, Entry::Domain, "13900000000".parse().expect("mobile"), pagination()).await.expect("target");
                    let original = Box::pin(services.assignment().assign(reader.clone(), operation(), command(&input(&candidate, &selected, Entry::Domain, "13900000000")))).await.expect("replacement");
                    let changed = Box::pin(services.role().update_workforce(reader.clone(), operation(), selected.id(), serde_json::from_value(json!({"entry":"domain","changeId":selected.change_id(),"name":"Later definition","level":200,"permissions":[]})).expect("later definition"))).await.expect("later role history outside the selected operation");
                    let candidate = services.assignment().candidate(&reader, Entry::Domain, "13900000000".parse().expect("mobile"), pagination()).await.expect("restore target");
                    Box::pin(services.assignment().assign(reader.clone(), operation(), command(&input(&candidate, first.role(), Entry::Domain, "13900000000")))).await.expect("healthy current default role");
                    let selected = || Filter { operation_id: Some(original.operation_id()), ..filter() };
                    assert_eq!(services.assignment().list(&reader, selected()).await.expect("complete pre-fault history").count(), 1);
                    let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("post-startup isolated damage");
                    match case {
                        "grant" => { owner.client().execute("DELETE FROM access.grants WHERE id=$1", &[&grant.id()]).await.expect("orphan cannot disappear through an inner join"); }
                        "predecessor" => { owner.client().execute("DELETE FROM access.assignments WHERE id=$1", &[&first.id()]).await.expect("missing first assignment outside the selected operation"); }
                        "proof" => { owner.client().execute("UPDATE access.assignments SET seal_key_id=99 WHERE id=$1", &[&original.id()]).await.expect("unavailable replacement seal key"); }
                        "subject" => { owner.client().execute("UPDATE access.assignments SET revision_id=gen_random_uuid() WHERE id=$1", &[&original.id()]).await.expect("missing fixed subject"); }
                        "operator" => { owner.client().execute("UPDATE access.assignments SET operator_id=gen_random_uuid() WHERE id=$1", &[&original.id()]).await.expect("missing fixed operator"); }
                        "name" => { owner.client().execute("UPDATE identity.revisions SET name=id_number WHERE id=$1", &[&first.subject().revision_id()]).await.expect("wrong field binding"); }
                        "id_number" => { owner.client().execute("UPDATE identity.revisions SET id_number=set_byte(id_number,octet_length(id_number)-1,get_byte(id_number,octet_length(id_number)-1)#1) WHERE id=$1", &[&first.subject().revision_id()]).await.expect("masked identity still requires authenticated number"); }
                        "definition" => { owner.client().execute("UPDATE access.changes SET name=$2 WHERE id=$1", &[&changed.change_id(), &original.role().name()]).await.expect("invalid no-op successor outside the page"); }
                        "selected_definition" => { owner.client().execute("UPDATE access.changes SET name='Damaged definition' WHERE id=$1", &[&original.role().change_id()]).await.expect("damage the returned fixed definition"); }
                        "future" => { owner.client().execute("UPDATE access.assignments SET assigned_at=clock_timestamp()+interval '1 hour' WHERE id=$1", &[&original.id()]).await.expect("future retained fact"); }
                        "query" => { owner.client().batch_execute(&format!("REVOKE SELECT ON identity.revisions FROM {}", database.user)).await.expect("ordinary query failure"); }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("fault committed");
                    let count: i64 = owner.client().query_one("SELECT count(*) FROM access.assignments", &[]).await.expect("independent post-fault count").get(0);
                    assert_eq!(count, if case == "predecessor" { 3 } else { 4 });
                    if matches!(case, "grant" | "predecessor" | "definition") {
                        assert_eq!(services.assignment().list(&reader, selected()).await.expect("unconsumed history does not invalidate fixed projection").count(), 1);
                        assert!(services.ready().await.is_ok());
                        if case != "definition" { assert!(services.grant().verify(owner.client()).await.is_err(), "startup rejects the damaged retained grant history"); }
                        else { assert!(crate::history::inspect::<crate::access::role::state::State>(owner.client(), services.app().keys(), context.sample().await.expect("verification time")).await.is_err(), "startup rejects damaged role history"); }
                        assert_eq!(owner.client().query_one("SELECT count(*) FROM access.assignments", &[]).await.expect("fixed projection does not repair history").get::<_, i64>(0), count);
                        owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                        assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                        return;
                    }
                    let error = services.assignment().list(&reader, selected()).await.err().expect("entire history page withheld");
                    assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                    assert_eq!(crate::runtime::work::Failure::unreliable(&error), case != "query", "{case}: {error:?}");
                    assert_eq!(services.ready().await.is_err(), case != "query");
                    for secret in ["Test Person", "000000200002290010", "13900000000"] { assert!(!format!("{error:?}").contains(secret)); }
                    if case == "query" {
                        owner.client().batch_execute(&format!("GRANT SELECT ON identity.revisions TO {}", database.user)).await.expect("restore isolated permission");
                        assert_eq!(services.assignment().list(&reader, selected()).await.expect("fresh read after technical recovery").count(), 1);
                    }
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.assignments", &[]).await.expect("no repair or mutation by reading").get::<_, i64>(0), count);
                    owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded history integrity case");
            }
        }));
    }

    #[test]
    fn history_batches_full_pages_without_per_person_round_trips() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::{
            access::appointment::tests::{mobile, register_numbered},
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
            Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await;
            for index in 10..109 {
                timeout(Duration::from_secs(60), Box::pin(async {
                    Box::pin(register_numbered(&database, &services, &mut supplier, index)).await;
                    Box::pin(admit(&services, &reader, Entry::Domain, &mobile(index), role::Kind::Default)).await;
                })).await.expect("bounded account and admission preparation");
            }
            timeout(Duration::from_secs(60), Box::pin(async {
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let rows = observer.client().query("SELECT id, grant_id, person_id, revision_id, operator_id, role_id, operation_id FROM access.assignments ORDER BY assigned_at DESC, id DESC", &[]).await.expect("committed full page before reading");
                assert_eq!(rows.len(), 100);
                assert_eq!(observer.client().query_one("SELECT count(DISTINCT person_id) FROM access.assignments", &[]).await.expect("distinct actual people").get::<_, i64>(0), 100);
                let mut baseline = None;
                for size in [1, 100] {
                    proxy.arm(Fault::DelayedStateQueries);
                    let started = std::time::Instant::now();
                    let result = services.assignment().list(&reader, Filter { pagination: Pagination::new(1, size).expect("page"), ..filter() }).await;
                    let queries = proxy.delayed_queries();
                    eprintln!("Assignment history size={size}: {:?}; queries delayed by 8 ms: {queries}", started.elapsed());
                    let page = result.expect("complete history within the existing read deadline");
                    assert!(queries > 0, "the delayed history transaction was observed");
                    assert_eq!(*baseline.get_or_insert(queries), queries, "a full page uses the same queries as a single row");
                    assert_eq!(page.count(), 100);
                    assert_eq!(page.assignments().len(), usize::try_from(size).expect("page size"));
                    for (assignment, row) in page.assignments().iter().zip(&rows) {
                        assert_eq!(assignment.id(), row.get("id"));
                        assert_eq!(assignment.grant_id(), row.get("grant_id"));
                        assert_eq!(assignment.operation_id(), row.get("operation_id"));
                        assert_eq!(assignment.role().id(), row.get("role_id"));
                        assert_eq!(assignment.subject().person_id(), row.get("person_id"));
                        assert_eq!(assignment.subject().revision_id(), row.get("revision_id"));
                        assert_eq!(assignment.operator().revision_id(), row.get("operator_id"));
                        assert!(matches!(assignment.subject().name(), "H*" | "T*"));
                    }
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.assignments", &[]).await.expect("reading preserves facts").get::<_, i64>(0), 100);
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded full history acceptance");
        }));
    }

    #[test]
    fn history_orders_personal_and_global_pages_by_time_and_id() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let reader = register(&database, &services, &mut supplier).await;
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await;
                let (grant, first) = Box::pin(admit(&services, &reader, Entry::Domain, "13900000000", role::Kind::Default)).await;
                let role = Box::pin(services.role().create_workforce(reader.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"Same-time role","level":200,"permissions":[]})).expect("role"))).await.expect("alternate role");
                let high: super::Id = serde_json::from_value(json!("ffffffff-ffff-4fff-8fff-ffffffffffff")).expect("high ID");
                let low: super::Id = serde_json::from_value(json!("00000000-0000-4000-8000-000000000001")).expect("low ID");
                let owner = database.connections("postgres").recover().await.expect("isolated ordering fixture");
                owner.client().batch_execute("BEGIN").await.expect("same-time fixture transaction");
                let now = context.sample().await.expect("fixture time");
                let mut state = crate::history::find::<crate::access::grant::state::State>(owner.client(), services.app().keys(), services.app().anchors(), crate::history::Read::Locked, grant.id(), now).await.expect("verified initial state").expect("state");
                for (id, before, after) in [(high, first.role(), &role), (low, &role, first.role())] {
                    let operation = operation();
                    let change = crate::access::grant::transition::assign(&state, before, after, crate::access::grant::transition::Assignment { id, entry: Entry::Domain, operator_id: first.operator().revision_id(), revision_id: first.subject().revision_id() }, operation, now).expect("legal replacement fixture");
                    let (next, record) = crate::history::Seed::change(change, services.app().keys()).expect("sealed replacement fixture");
                    crate::access::grant::tests::fixture_completion(owner.client(), services.grant(), super::operation::COMPLETIONS, "assign", Uuid::from_bytes(*grant.id().as_bytes()).to_string(), first.operator().revision_id(), operation, now).await;
                    crate::history::Seed::persist_record(owner.client(), services.app().keys(), &record).await.expect("fixture record");
                    crate::history::Seed::persist_state(owner.client(), &next, Some(&state)).await.expect("paired fixture state");
                    state = next;
                }
                owner.client().batch_execute("COMMIT").await.expect("ordering fixture committed");
                crate::history::Seed::anchor(services.app().anchors(), &state).expect("fixture state anchor");
                let facts = owner.client().query("SELECT id,assigned_at FROM access.assignments WHERE grant_id=$1 ORDER BY sequence DESC", &[&grant.id()]).await.expect("independent order before read");
                assert_eq!(facts[0].get::<_, super::Id>(0), low); assert_eq!(facts[1].get::<_, super::Id>(0), high);
                assert_eq!(facts[0].get::<_, chrono::DateTime<chrono::Utc>>(1), facts[1].get::<_, chrono::DateTime<chrono::Utc>>(1));
                let personal = services.assignment().list(&reader, Filter { person_id: Some(first.subject().person_id()), ..filter() }).await.expect("personal time and ID order");
                assert_eq!(personal.assignments().iter().map(super::Assignment::id).collect::<Vec<_>>(), [high, low, first.id()]);
                let global = services.assignment().list(&reader, filter()).await.expect("global time and ID order");
                assert_eq!(global.assignments()[0].id(), high); assert_eq!(global.assignments()[1].id(), low);
                owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded stable ordering acceptance");
        }));
    }

    #[test]
    fn history_withholds_unfinished_reads_and_releases_only_the_original_backend() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await;
                let observer = database.connections("postgres").recover().await.expect("independent backend observer");
                let unrelated = database.connections(&database.user).recover().await.expect("unrelated connection");
                unrelated.client().batch_execute("BEGIN; SELECT pg_catalog.pg_advisory_xact_lock(2147483647, 2147483647)").await.expect("unrelated transaction and lock");
                let unrelated_pid: i32 = unrelated.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("unrelated identity").get(0);
                for case in ["cancel", "count timeout", "page timeout", "commit", "finish"] {
                    proxy.arm(if case == "page timeout" { Fault::StalledRolePage } else { Fault::StalledRoleCount });
                    let service = services.assignment().clone(); let actor = member.clone();
                    let task = tokio::spawn(async move { service.list(&actor, filter()).await });
                    let pid = timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("exact history reader PID");
                    assert_ne!(pid, unrelated_pid);
                    if case == "cancel" {
                        task.abort(); assert!(task.await.is_err_and(|error| error.is_cancelled()));
                    } else {
                        match case {
                            "commit" => { proxy.arm(Fault::LostReadCommit); proxy.release(); }
                            "finish" => { proxy.arm(Fault::StalledReadCommit); proxy.release(); }
                            "count timeout" | "page timeout" => {}
                            _ => unreachable!(),
                        }
                        if matches!(case, "commit" | "finish") {
                            timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("selected finalization boundary");
                        }
                        let error = timeout(Duration::from_secs(5), task).await.expect("bounded history read").expect("reader task").err().expect("incomplete page withheld");
                        assert_eq!(error.failure(), super::Failure::Unavailable, "{case}: {error:?}");
                    }
                    proxy.release();
                        timeout(Duration::from_secs(3), async {
                            while observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("original backend lifetime").get::<_, bool>(0) { tokio::task::yield_now().await; }
                        }).await.expect("original backend ended before fixture cleanup");
                        services.ready().await.expect("ordinary read failure preserves admission");
                        assert_eq!(services.assignment().list(&member, filter()).await.expect("next independent complete page").count(), 1);
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND state='idle in transaction') AND EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND locktype='advisory' AND granted)", &[&unrelated_pid]).await.expect("unrelated work survives read cleanup").get::<_, bool>(0));
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.assignments", &[]).await.expect("failures cannot write or replay assignments").get::<_, i64>(0), 1);
                }
                unrelated.client().batch_execute("ROLLBACK").await.expect("unrelated transaction ended"); unrelated.terminate().await.expect("unrelated connection ended");
                services.close(); services.drain().await.expect("drained"); let _ = context.stop().await;
                timeout(Duration::from_secs(3), async {
                    while observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND usename=$1)", &[&database.user]).await.expect("all runtime backends").get::<_, bool>(0) { tokio::task::yield_now().await; }
                }).await.expect("clock failure and stop end all runtime backends before fixture cleanup");
                observer.terminate().await.expect("observer ended"); proxy.stop().await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded read finalization acceptance");
        }));
    }

    #[test]
    fn domain_replacement_preserves_suspension_and_replay_cannot_change_rejoined_membership() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(50), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = register(&database, &services, &mut supplier).await;
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await;
                let (original_grant, first) = Box::pin(admit(&services, &manager, Entry::Domain, "13900000000", role::Kind::Default)).await;
                let selected = services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"Badge Coordinators","level":500,"permissions":["workforce.admission"]})).expect("role input")).await.expect("domain role");
                let candidate = services.assignment().candidate(&manager, Entry::Domain, "13900000000".parse().expect("mobile"), pagination()).await.expect("higher institutional target remains a lower domain member");
                assert_eq!(candidate.count(), 2); assert!(candidate.roles().iter().all(|role| role.kind() != role::Kind::Full));
                assert!(matches!(services.assignment().candidate(&manager, Entry::Institution, "13900000000".parse().expect("mobile"), pagination()).await, Err(Error::Role(role::Error::Denied))));
                assert!(matches!(services.assignment().candidate(&manager, Entry::Domain, "13800000000".parse().expect("mobile"), pagination()).await, Err(Error::Absent)));
                let body = input(&candidate, &selected, Entry::Domain, "13900000000");
                let subject = candidate.identity();
                let suspended = Box::pin(services.grant().suspend(manager.clone(), operation(), original_grant.id(), serde_json::from_value(json!({"target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":"13900000000"}})).expect("pause input"))).await.expect("actual domain pause");
                let operation = operation();
                let assigned = Box::pin(services.assignment().assign(manager.clone(), operation, command(&body))).await.expect("paused target may change roles");
                assert_eq!(assigned.sequence(), 3); assert_eq!(assigned.grant_id(), original_grant.id());
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND id=$1 AND assignment_id=$2) AND EXISTS(SELECT 1 FROM access.suspensions WHERE id=$3) AND NOT EXISTS(SELECT 1 FROM access.resumptions WHERE id=$3) AND (SELECT count(*) FROM access.assignments WHERE grant_id=$1)=2", &[&original_grant.id(), &assigned.id(), &suspended.id()]).await.expect("replacement preserves exact pause").get::<_, bool>(0));
                assert_eq!(services.grant().read_latest(&institution).await.expect("paused grant").status(), crate::access::grant::Status::Suspended);
                assert_eq!(services.grant().read_role(&institution).await.expect("paused member can inspect role").id(), selected.id());
                Box::pin(services.grant().terminate(institution.clone(), self::operation(), original_grant.id())).await.expect("real paused-member exit");
                let (rejoined, new_first) = Box::pin(admit(&services, &manager, Entry::Domain, "13900000000", role::Kind::Default)).await;
                assert_ne!(rejoined.id(), original_grant.id()); assert_ne!(new_first.id(), first.id());
                assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND id=$1 AND assignment_id=$2) AND NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND id=$3) AND (SELECT count(*) FROM access.assignments)=4", &[&rejoined.id(), &new_first.id(), &original_grant.id()]).await.expect("independent state before replay").get::<_, bool>(0));
                let replay = Box::pin(services.assignment().assign(manager.clone(), operation, command(&body))).await.expect("exact original after rejoining");
                assert_eq!(replay.id(), assigned.id()); assert_eq!(replay.grant_id(), original_grant.id());
                assert!(matches!(Box::pin(services.assignment().assign(manager.clone(), self::operation(), command(&body))).await, Err(Error::TargetChanged)));
                assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND id=$1 AND assignment_id=$2) AND (SELECT count(*) FROM access.assignments)=4", &[&rejoined.id(), &new_first.id()]).await.expect("replay cannot touch new membership").get::<_, bool>(0));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                assert_eq!(Box::pin(services.assignment().assign(manager, operation, command(&body))).await.expect("original across restart").id(), assigned.id());
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("domain replacement lifecycle deadline");
        }));
    }

    #[test]
    fn candidate_withholds_unfinished_reads_and_ends_the_original_backend() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["cancel", "timeout", "lost commit"] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await; let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await; register(&database, &services, &mut supplier).await;
                    let (grant, first) = Box::pin(admit(&services, &manager, Entry::Institution, "13800000000", role::Kind::Default)).await;
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    proxy.arm(Fault::StalledGrant); let service = services.assignment().clone(); let actor = manager.clone();
                    let reader = tokio::spawn(async move { service.candidate(&actor, Entry::Institution, "13800000000".parse().expect("mobile"), pagination()).await });
                    let pid = timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("current membership read after identity snapshot");
                    match case { "cancel" => reader.abort(), "lost commit" => proxy.arm(Fault::LostReadCommit), "timeout" => {}, _ => unreachable!() }
                    if case != "timeout" { proxy.release(); }
                    let result = timeout(Duration::from_secs(6), reader).await.expect("bounded reader completion");
                    if case == "timeout" { proxy.release(); }
                    if case == "cancel" { assert!(result.is_err_and(|error|error.is_cancelled())); }
                    else { assert_eq!(result.expect("reader task").err().expect("unfinished candidate is withheld").failure(),super::Failure::Unavailable); }
                    if case=="lost commit" { timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("selected finalization fault occurred"); proxy.release(); }
                    if matches!(case,"cancel"|"timeout") {
                        timeout(Duration::from_secs(3),async {loop {
                            if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",&[&pid]).await.expect("original backend lifetime").get::<_,bool>(0){break;}
                            tokio::task::yield_now().await;
                        }}).await.expect("cancelled or expired read releases its backend");
                    }
                    assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.assignments WHERE grant_id=$1)=1 AND EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND id=$1 AND assignment_id=$2)",&[&grant.id(),&first.id()]).await.expect("candidate failure does not write").get::<_,bool>(0));
                    services.ready().await.expect("ordinary read failure keeps admission open"); assert_eq!(services.assignment().candidate(&manager,Entry::Institution,"13800000000".parse().expect("mobile"),pagination()).await.expect("later independent read").assignment_id(),first.id());
                    observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("candidate finalization scenario deadline");
            }
        }));
    }

    #[test]
    fn candidate_identity_current_assignment_and_role_page_share_one_snapshot() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::StalledGrant, Fault::StalledRoleStates] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await; let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = crate::access::role::tests::start_snapshot(authentication::proxied(&database, &proxy), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await; register(&database, &services, &mut supplier).await;
                    let (grant, first) = Box::pin(admit(&services, &manager, Entry::Institution, "13800000000", role::Kind::Default)).await;
                    let before = services.assignment().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), pagination()).await.expect("initial confirmation");
                    proxy.arm(fault); let service = services.assignment().clone(); let actor = manager.clone();
                    let read = tokio::spawn(async move { service.candidate(&actor, Entry::Institution, "13800000000".parse().expect("mobile"), pagination()).await });
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual member read or authenticated role collection");
                    let added = Box::pin(services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"New choice","level":200,"permissions":[]})).expect("role input"))).await.expect("concurrent new definition");
                    let changed = Box::pin(services.assignment().assign(manager.clone(), operation(), command(&input(&before, &added, Entry::Institution, "13800000000")))).await.expect("concurrent member replacement");
                    Box::pin(services.revocation().revoke(manager.clone(), operation(), crate::identity::revocation::Scope::Current)).await.expect("concurrent session loss");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    proxy.release(); let error = read.await.expect("reader").err().expect("new role anchor retries the full candidate and its session authorization");
                    assert_eq!(error.failure(), super::Failure::Invalid);
                    assert!(matches!(error, Error::Role(role::Error::Current(crate::identity::current::Error::Invalid))), "{error:?}");
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND id=$1 AND assignment_id=$2 AND role_id=$3)", &[&grant.id(),&changed.id(),&added.id()]).await.expect("later assignment committed").get::<_,bool>(0));
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.assignments WHERE id=$1 AND grant_id=$2) AND EXISTS(SELECT 1 FROM access.roles WHERE id=$3 AND active)", &[&first.id(), &grant.id(), &added.id()]).await.expect("original assignment and new anchor fact remain committed").get::<_,bool>(0));
                    assert_eq!(services.assignment().candidate(&manager,Entry::Institution,"13800000000".parse().expect("mobile"),pagination()).await.err().expect("next request sees session revocation").failure(),super::Failure::Invalid);
                    let current = role::tests::actor(&database, &services, &mut supplier).await;
                    let latest = services.assignment().candidate(&current, Entry::Institution, "13800000000".parse().expect("mobile"), pagination()).await.expect("fresh authorized snapshot");
                    assert_eq!(latest.assignment_id(), changed.id()); assert_eq!(latest.role().id(), added.id());
                    assert_eq!(latest.count(), 3); assert!(latest.roles().iter().any(|role| role.id() == added.id())); observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("candidate snapshot scenario deadline");
            }
        }));
    }

    #[test]
    fn replacement_checks_the_selected_authority_and_rejects_restored_confirmations() {
        use crate::access::assignment::Failure;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(50), Box::pin(async {
                let database = Box::pin(Database::initialized(Arc::clone(&server))).await; let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let manager = Box::pin(actor(&database, &services, &mut supplier)).await; let member = Box::pin(register(&database, &services, &mut supplier)).await;
                Box::pin(admit(&services, &manager, Entry::Institution, "13800000000", role::Kind::Full)).await;
                let authority = Box::pin(services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Coordinators","level":700,"permissions":["workforce.roles","workforce.assignments","workforce.admission"]})).expect("authority input"))).await.expect("limited domain authority");
                Box::pin(admit(&services, &member, Entry::Domain, "13900000000", role::Kind::Custom)).await;
                assert_eq!(Box::pin(services.grant().read_role(&manager)).await.expect("actual domain authority").id(), authority.id());
                assert_eq!(Box::pin(services.assignment().candidate(&manager, Entry::Domain, "13800000000".parse().expect("mobile"), pagination())).await.err().expect("higher domain target hidden").failure(), Failure::Absent);
                let candidate = Box::pin(services.assignment().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), pagination())).await.expect("institution may replace full domain role");
                let default = candidate.roles().iter().find(|role| role.kind()==role::Kind::Default).expect("default");
                Box::pin(services.assignment().assign(manager.clone(), operation(), command(&input(&candidate, default, Entry::Institution, "13800000000")))).await.expect("institutional source ignores domain rank");
                let selected = Box::pin(services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Allowed","level":500,"permissions":["workforce.admission"]})).expect("role input"))).await.expect("allowed role");
                let outside = Box::pin(services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Numbers","level":400,"permissions":["workforce.numbers"]})).expect("role input"))).await.expect("unowned permission");
                let equal = Box::pin(services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Peers","level":700,"permissions":[]})).expect("role input"))).await.expect("equal rank");
                let candidate = Box::pin(services.assignment().candidate(&manager, Entry::Domain, "13800000000".parse().expect("mobile"), pagination())).await.expect("domain candidate");
                assert_eq!(candidate.count(),2); assert!(candidate.roles().iter().all(|role| role.id()==selected.id() || role.kind()==role::Kind::Default));
                for forbidden in [&outside, &equal] {
                    assert!(matches!(Box::pin(services.assignment().assign(manager.clone(), operation(), command(&input(&candidate, forbidden, Entry::Domain, "13800000000")))).await, Err(Error::Denied)));
                }
                let body = input(&candidate, &selected, Entry::Domain, "13800000000"); let original_id = operation();
                let assigned = Box::pin(services.assignment().assign(manager.clone(), original_id, command(&body))).await.expect("domain assignment");
                let mut other_source = body.clone(); other_source["entry"] = json!("institution");
                assert!(matches!(Box::pin(services.assignment().assign(manager.clone(), original_id, command(&other_source))).await, Err(Error::Mismatch)), "both basic permissions cannot change the original source");
                let candidate = Box::pin(services.assignment().candidate(&manager, Entry::Domain, "13800000000".parse().expect("mobile"), pagination())).await.expect("fresh confirmation");
                let default = candidate.roles().iter().find(|role| role.kind()==role::Kind::Default).expect("default");
                let stale = input(&candidate, default, Entry::Domain, "13800000000");
                Box::pin(services.assignment().assign(manager.clone(), operation(), command(&stale))).await.expect("move away");
                let moved = Box::pin(services.assignment().candidate(&manager, Entry::Domain, "13800000000".parse().expect("mobile"), pagination())).await.expect("intermediate assignment");
                Box::pin(services.assignment().assign(manager.clone(), operation(), command(&input(&moved, &selected, Entry::Domain, "13800000000")))).await.expect("move back to same role with a new assignment");
                assert!(matches!(Box::pin(services.assignment().assign(manager.clone(), operation(), command(&stale))).await, Err(Error::TargetChanged)));
                let candidate = Box::pin(services.assignment().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), pagination())).await.expect("definition confirmation");
                let stale = input(&candidate, &outside, Entry::Institution, "13800000000");
                let changed = Box::pin(services.role().update_workforce(manager.clone(), operation(), outside.id(), serde_json::from_value(json!({"entry":"institution","changeId":outside.change_id(),"name":"Numbers Updated","level":401,"permissions":["workforce.numbers"]})).expect("update"))).await.expect("move definition away");
                let restored = Box::pin(services.role().update_workforce(manager.clone(), operation(), outside.id(), serde_json::from_value(json!({"entry":"institution","changeId":changed.change_id(),"name":"Numbers","level":400,"permissions":["workforce.numbers"]})).expect("restore"))).await.expect("restore visible definition");
                assert_ne!(restored.change_id(),outside.change_id());
                assert!(matches!(Box::pin(services.assignment().assign(manager.clone(), operation(), command(&stale))).await, Err(Error::RoleChanged)));
                let suspension = Box::pin(services.suspension().suspend(manager.clone(), operation(), candidate.identity().id(), serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"})).expect("pause input"))).await.expect("real account pause");
                let paused = Box::pin(services.assignment().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), pagination())).await.expect("paused account remains manageable");
                Box::pin(services.assignment().assign(manager.clone(), operation(), command(&input(&paused, &restored, Entry::Institution, "13800000000")))).await.expect("replace paused account's domain role");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.suspensions WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE id=$1) AND EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND id=$2 AND role_id=$3) AND (SELECT count(*) FROM access.assignments WHERE grant_id=$2)=6", &[&suspension.id(),&paused.grant_id(),&restored.id()]).await.expect("exact pause and permanent assignment chain").get::<_,bool>(0));
                Box::pin(services.role().delete_workforce(manager.clone(),operation(),selected.id(),serde_json::from_value(json!({"entry":"institution","changeId":selected.change_id()})).expect("deletion"))).await.expect("unused original role may be deleted");
                let replay = Box::pin(services.assignment().assign(manager.clone(), original_id, command(&body))).await.expect("original retains deleted definition and fixed identity");
                assert_eq!(replay.id(),assigned.id()); assert_eq!(replay.role().change_id(),selected.change_id());
                Box::pin(services.cancellation().cancel_account(manager.clone(), operation(), paused.identity().id(), serde_json::from_value(json!({"personId":paused.identity().person_id(),"revisionId":paused.identity().revision_id(),"mobile":"13800000000"})).expect("cancellation input"))).await.expect("real cancellation after replacements");
                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND id=$1) AND (SELECT count(*) FROM access.assignments WHERE grant_id=$1)=6 AND EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$2)", &[&paused.grant_id(),&paused.identity().person_id()]).await.expect("cancellation retains every assignment before replay").get::<_,bool>(0));
                assert_eq!(Box::pin(services.assignment().assign(manager.clone(), original_id, command(&body))).await.expect("original survives cancellation").id(),assigned.id());
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                assert_eq!(Box::pin(services.assignment().assign(manager, original_id, command(&body))).await.expect("retained original after restart").id(),assigned.id());
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("authority and confirmation deadline");
        }));
    }

    #[test]
    fn replacement_serializes_role_changes_and_member_exit_in_both_orders() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for entry in [Entry::Institution, Entry::Domain] {
                for role_change in [true, false] {
                    for assignment_first in [true, false] {
                        timeout(Duration::from_secs(35), Box::pin(async {
                            let database = Database::initialized(Arc::clone(&server)).await; let mut supplier = Supplier::new().await;
                            let proxy = Proxy::start(&server).await;
                            let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                            let institution = actor(&database, &services, &mut supplier).await; let member = register(&database, &services, &mut supplier).await;
                            let (manager, subject, mobile) = match entry {
                                Entry::Institution => (institution, member, "13800000000"),
                                Entry::Domain => { Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await; (member, institution, "13900000000") }
                            };
                            let (grant, _) = Box::pin(admit(&services, &manager, entry, mobile, role::Kind::Default)).await;
                            let selected = services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":entry.as_ref(),"name":"Before","level":200,"permissions":[]})).expect("role input")).await.expect("selected definition");
                            let candidate = services.assignment().candidate(&manager, entry, mobile.parse().expect("mobile"), pagination()).await.expect("candidate");
                            let body = input(&candidate, &selected, entry, mobile); let assignment_id = operation(); let competing_id = operation();
                            let changed = json!({"entry":entry.as_ref(),"changeId":selected.change_id(),"name":"After","level":201,"permissions":[]});
                            let grant_id = grant.id(); let role_id = selected.id();
                            let observer = database.connections(&database.user).recover().await.expect("independent observer");
                            proxy.arm(Fault::DelayedCommit);
                            let service = services.assignment().clone(); let grants = services.grant().clone(); let roles = services.role().clone();
                            let first_actor = manager.clone(); let first_subject = subject.clone(); let first_input = command(&body); let update = serde_json::from_value(changed.clone()).expect("update input");
                            let first = tokio::spawn(async move {
                                if assignment_first { Box::pin(service.assign(first_actor, assignment_id, first_input)).await.expect("replacement first"); }
                                else if role_change { Box::pin(roles.update_workforce(first_actor, competing_id, role_id, update)).await.expect("role update first"); }
                                else { Box::pin(grants.terminate(first_subject, competing_id, grant_id)).await.expect("exit first"); }
                            });
                            let blocker = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("first operation reached its actual commit");
                            let service = services.assignment().clone(); let grants = services.grant().clone(); let roles = services.role().clone();
                            let second_actor = manager.clone(); let second_input = command(&body); let update = serde_json::from_value(changed).expect("update input");
                            let second = tokio::spawn(async move {
                                if !assignment_first {
                                    let result = Box::pin(service.assign(second_actor, assignment_id, second_input)).await;
                                    if role_change { assert!(matches!(result, Err(Error::RoleChanged))); }
                                    else { assert!(matches!(result, Err(Error::Absent))); }
                                } else if role_change { Box::pin(roles.update_workforce(second_actor, competing_id, role_id, update)).await.expect("role update after assignment"); }
                                else { Box::pin(grants.terminate(subject, competing_id, grant_id)).await.expect("exit after assignment"); }
                            });
                            timeout(Duration::from_secs(3), async { loop {
                                if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND usename=$1 AND wait_event_type='Lock' AND $2=ANY(pg_blocking_pids(pid)))", &[&database.user,&blocker]).await.expect("actual second operation waits on the first backend").get::<_, bool>(0) { break; }
                                tokio::task::yield_now().await;
                            }}).await.expect("business operations cannot cross their coordination boundary");
                            proxy.release(); first.await.expect("first worker"); second.await.expect("second worker");
                            let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.assignments WHERE grant_id=$1) AS assignments,(SELECT count(*) FROM access.terminations WHERE id=$1) AS terminations,(SELECT count(*) FROM access.grants WHERE termination IS NULL AND id=$1) AS members,(SELECT count(*) FROM access.changes WHERE operation_id=$2) AS changes", &[&grant_id,&competing_id]).await.expect("independent ordered outcomes before replay");
                            assert_eq!(row.get::<_, i64>("assignments"), if assignment_first { 2 } else { 1 });
                            assert_eq!(row.get::<_, i64>("terminations"), i64::from(!role_change)); assert_eq!(row.get::<_, i64>("members"), i64::from(role_change)); assert_eq!(row.get::<_, i64>("changes"), i64::from(role_change));
                            if assignment_first {
                                let original = Box::pin(services.assignment().assign(manager, assignment_id, command(&body))).await.expect("original survives later definition change or exit");
                                assert_eq!(original.role().change_id(), selected.change_id()); assert_eq!(original.sequence(), 2);
                            }
                            observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                            assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                        })).await.expect("replacement competition scenario deadline");
                    }
                }
            }
        }));
    }

    #[test]
    fn accepted_replacement_uses_its_fixed_operator_during_recovery() {
        use fixture::database_proxy::Proxy;

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for entry in [Entry::Institution, Entry::Domain] {
                for damaged in [false, true] {
                    timeout(Duration::from_secs(35), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await; let mut supplier = Supplier::new().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                        let institution = actor(&database, &services, &mut supplier).await; let member = register(&database, &services, &mut supplier).await;
                        let (manager, mobile) = match entry {
                            Entry::Institution => (institution, "13800000000"),
                            Entry::Domain => { Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await; (member, "13900000000") }
                        };
                        let (grant, _) = Box::pin(admit(&services, &manager, entry, mobile, role::Kind::Default)).await;
                        let selected = services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":entry.as_ref(),"name":"Operators","level":200,"permissions":[]})).expect("role input")).await.expect("selected definition");
                        let candidate = services.assignment().candidate(&manager, entry, mobile.parse().expect("mobile"), pagination()).await.expect("candidate");
                        let body = input(&candidate, &selected, entry, mobile); let operation = operation();
                        let identity = services.current().read(&manager).await.expect("accepted operator identity");
                        let request = services.assignment().assign(manager.clone(), operation, command(&body));
                        let result = if damaged { Box::pin(crate::recovery::after_revision_loss(&database, &proxy, &manager, request)).await }
                            else { Box::pin(crate::recovery::after_revocation(&database, &proxy, services.revocation(), &manager, request)).await };
                        let observer = database.connections(&database.user).recover().await.expect("observer");
                        let row = observer.client().query_one("SELECT assignment.id,assignment.operator_id,assignment.revision_id,assignment.entry,assignment.sequence,member.assignment_id,member.role_id FROM access.assignments assignment JOIN access.grants member ON member.id=assignment.grant_id AND member.termination IS NULL WHERE assignment.operation_id=$1 AND assignment.entry IS NOT NULL", &[&operation]).await.expect("independent original facts before replay");
                        assert_eq!(row.get::<_, super::Id>("id"), row.get("assignment_id")); assert_eq!(row.get::<_, role::Id>("role_id"), selected.id());
                        assert_eq!(row.get::<_, crate::identity::person::revision::Id>("operator_id"), identity.revision_id());
                        assert_eq!(row.get::<_, crate::identity::person::revision::Id>("revision_id"), candidate.identity().revision_id());
                        assert_eq!(row.get::<_, Entry>("entry"), entry); assert_eq!(row.get::<_, i64>("sequence"), 2);
                        if damaged {
                            let error = result.err().expect("missing original identity prevents delivery");
                            assert!(matches!(&error, Error::Command(crate::kernel::Error::Unknown(fault)) if fault.later().any(|(stage,cause)| stage == crate::kernel::Stage::Recovery && matches!(cause, crate::kernel::FaultKind::Assignment(Error::Read(super::ReadError::Revision(crate::identity::person::revision::ReadError::Missing)))))), "{error:?}"); assert!(services.ready().await.is_err());
                        } else {
                            let assigned = result.expect("accepted assignment survives later revocation"); assert_eq!(assigned.id(), row.get("id")); assert_eq!(assigned.grant_id(), grant.id());
                            assert_eq!(Box::pin(services.assignment().assign(manager, operation, command(&body))).await.err().expect("new request rechecks the revoked session").failure(), super::Failure::Invalid);
                        }
                        observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                        assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                    })).await.expect("accepted recovery scenario deadline");
                }
            }
        }));
    }

    #[test]
    fn replacement_recovers_commit_faults_and_outlives_its_waiter() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for entry in [Entry::Institution, Entry::Domain] {
                for fault in [Fault::LostCommit, Fault::RolledBack, Fault::UnknownCommit, Fault::ReadCommitTermination, Fault::DelayedCommit] {
                    timeout(Duration::from_secs(35), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await; let mut supplier = Supplier::new().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                        let institution = actor(&database, &services, &mut supplier).await; let member = register(&database, &services, &mut supplier).await;
                        let (manager, mobile) = match entry {
                            Entry::Institution => (institution, "13800000000"),
                            Entry::Domain => { Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await; (member, "13900000000") }
                        };
                        let (grant, first) = Box::pin(admit(&services, &manager, entry, mobile, role::Kind::Default)).await;
                        let selected = services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":entry.as_ref(),"name":"Operators","level":200,"permissions":[]})).expect("role input")).await.expect("selected definition");
                        let candidate = services.assignment().candidate(&manager, entry, mobile.parse().expect("mobile"), pagination()).await.expect("candidate");
                        let body = input(&candidate, &selected, entry, mobile); let input = command(&body); let operation = operation();
                        let cancelled = matches!(fault, Fault::DelayedCommit); let rolled_back = matches!(fault, Fault::RolledBack); let unknown = matches!(fault, Fault::UnknownCommit); let terminated = matches!(fault, Fault::ReadCommitTermination);
                        proxy.arm(if terminated { Fault::DelayedCommit } else { fault });
                        let service = services.assignment().clone(); let actor = manager.clone();
                        let waiter = tokio::spawn(async move { service.assign(actor, operation, input).await });
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("fault reached business commit");
                        if terminated { proxy.arm(Fault::ReadCommitTermination); }
                        if cancelled {
                            waiter.abort(); let _ = waiter.await; services.close();
                            assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "cancelling the waiter cannot release accepted work");
                            proxy.release(); services.drain().await.expect("accepted replacement drained");
                        } else {
                            proxy.release();
                            let result = timeout(Duration::from_secs(15), waiter).await.expect("bounded outcome").expect("waiter");
                            if rolled_back { let error = result.err().expect("rolled back command"); assert!(matches!(error, Error::Command(crate::kernel::Error::Unavailable(_))), "{error:?}"); }
                            else if unknown { let error = result.err().expect("unknown command"); assert!(matches!(error, Error::Command(crate::kernel::Error::Unknown(_))), "{error:?}"); }
                            else { assert_eq!(result.expect("confirmed assignment").sequence(), 2); }
                            services.close(); services.drain().await.expect("drained");
                        }
                        context.stop().await.expect("context stopped");
                        let observer = database.connections(&database.user).recover().await.expect("independent observer");
                        observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                        observer.client().query_one("SELECT id FROM access.grants WHERE id=$1 FOR UPDATE", &[&grant.id()]).await.expect("original backend released its grant lock");
                        let row = observer.client().query_one("SELECT person.sequence,member.assignment_id,member.role_id,(SELECT count(*) FROM access.assignments WHERE grant_id=$1) AS count,(SELECT count(*) FROM access.grants WHERE person_id=person.id) AS grants FROM access.grants member JOIN identity.persons person ON person.id=member.person_id WHERE member.id=$1 AND member.termination IS NULL", &[&grant.id()]).await.expect("durable facts before replay");
                        assert_eq!(row.get::<_, i64>("sequence"), 1);
                        assert_eq!(row.get::<_, i64>("count"), if rolled_back { 1 } else { 2 });
                        assert_eq!(row.get::<_, i64>("grants"), 1);
                        assert_eq!(row.get::<_, role::Id>("role_id"), if rolled_back { first.role().id() } else { selected.id() });
                        let persisted: super::Id = row.get("assignment_id");
                        assert_eq!(persisted == first.id(), rolled_back);
                        observer.client().batch_execute("COMMIT").await.expect("observation ended"); observer.terminate().await.expect("observer ended"); proxy.stop().await;
                        let (context, services) = start(database.connections(&database.user), &supplier).await;
                        let original = Box::pin(services.assignment().assign(manager.clone(), operation, command(&body))).await.expect("explicit request recovers or follows confirmed rollback");
                        if !rolled_back { assert_eq!(original.id(), persisted); }
                        assert_eq!(original.sequence(), 2); assert_eq!(original.role().id(), selected.id());
                        if !cancelled && !rolled_back && !unknown && !terminated {
                            let mut changed = body.clone(); changed["target"]["mobile"] = json!("13700000000");
                            let (same, different) = tokio::join!(
                                services.assignment().assign(manager.clone(), operation, command(&body)),
                                services.assignment().assign(manager.clone(), operation, command(&changed))
                            );
                            assert_eq!(same.expect("concurrent original waiter").id(), original.id());
                            assert!(matches!(different, Err(Error::Mismatch)), "each waiter authenticates its own original input");
                        }
                        services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                        assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                    })).await.expect("replacement fault scenario deadline");
                }
            }
        }));
    }

    #[test]
    fn runtime_sql_pairs_assignment_records_with_grant_state_and_completion() {
        use tokio_postgres::error::SqlState;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for entry in [Entry::Institution, Entry::Domain] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let member = register(&database, &services, &mut supplier).await;
                    let (manager, mobile) = match entry {
                        Entry::Institution => (institution, "13800000000"),
                        Entry::Domain => {
                            Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", role::Kind::Full)).await;
                            (member, "13900000000")
                        }
                    };
                    let (grant, first) = Box::pin(admit(&services, &manager, entry, mobile, role::Kind::Default)).await;
                    let selected = services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":entry.as_ref(),"name":"Operators","level":200,"permissions":[]})).expect("role input")).await.expect("selected custom role");
                    let candidate = services.assignment().candidate(&manager, entry, mobile.parse().expect("mobile"), pagination()).await.expect("current member");
                    let revision = candidate.identity().revision_id();
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let operator: crate::identity::person::revision::Id = observer.client().query_one("SELECT person.revision_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1", &[&manager.session_id()]).await.expect("fixed manager").get(0);
                    for case in ["valid", "record only", "state only", "missing completion", "head", "version", "sequence", "fixed admission", "duplicate", "immutable assignment"] {
                        use crate::history::{self, Read, Seed, State as _};
                        let writer = database.connections(&database.user).recover().await.expect("runtime writer");
                        writer.client().batch_execute("BEGIN").await.expect("write transaction");
                        let now = context.sample().await.expect("time sampled after BEGIN"); let id = super::Id::allocate(); let operation = operation();
                        let original = history::find::<crate::access::grant::state::State>(writer.client(), services.app().keys(), services.app().anchors(), Read::Locked, grant.id(), now).await.expect("verified control state").expect("state");
                        let change = crate::access::grant::transition::assign(&original, first.role(), &selected, crate::access::grant::transition::Assignment { id, entry, operator_id: operator, revision_id: revision }, operation, now).expect("legal replacement control");
                        let (state, record) = Seed::change(change, services.app().keys()).expect("sealed control pair");
                        let data = state.fixture_data(); let mut sequence = state.sequence(); let mut version = state.version();
                        match case {
                            "version" => version += 1,
                            "sequence" => sequence = original.sequence(),
                            _ => (),
                        }
                        let state = Seed::state::<crate::access::grant::state::State>(data, sequence, version, services.app().keys()).expect("sealed fixture state");
                        if case != "missing completion" {
                            crate::access::grant::tests::fixture_completion(writer.client(), services.grant(), super::operation::COMPLETIONS, "assign", Uuid::from_bytes(*grant.id().as_bytes()).to_string(), operator, operation, now).await;
                        }
                        let result: Result<(), history::Error> = async {
                            if case == "immutable assignment" {
                                writer.client().execute("UPDATE access.assignments SET role_id=$2 WHERE id=$1", &[&first.id(), &selected.id()]).await?;
                            } else {
                                if case != "state only" { Seed::persist_record(writer.client(), services.app().keys(), &record).await?; }
                                if case == "duplicate" { Seed::persist_record(writer.client(), services.app().keys(), &record).await?; }
                                if matches!(case, "head" | "fixed admission") {
                                    let granted_at = state.granted_at() - if case == "fixed admission" { chrono::Duration::microseconds(1) } else { chrono::Duration::zero() };
                                    let changed_at = state.changed_at() + if case == "head" { chrono::Duration::microseconds(1) } else { chrono::Duration::zero() };
                                    writer.client().execute("UPDATE access.grants SET granted_at=$2,role_id=$3,assignment_id=$4,suspension_id=$5,epoch=$6,changed_at=$7,sequence=$8,version=$9,seal=$10,seal_key_id=$11,seal_version=$12 WHERE id=$1", &[&grant.id(), &granted_at, &state.role_id(), &state.assignment_id(), &state.suspension_id(), &state.epoch(), &changed_at, &state.sequence(), &state.version(), &state.seal().tag(), &state.seal().key_id(), &state.seal().version()]).await?;
                                } else if case != "record only" { Seed::persist_state(writer.client(), &state, Some(&original)).await?; }
                            }
                            writer.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await?;
                            Ok(())
                        }.await;
                        if matches!(case, "valid" | "record only") {
                            result.unwrap_or_else(|error| panic!("{entry:?}: {case}: {error:?}"));
                            if case == "valid" { history::inspect::<crate::access::grant::state::State>(writer.client(), services.app().keys(), now).await.expect("control is complete history"); }
                            else { assert!(history::inspect::<crate::access::grant::state::State>(writer.client(), services.app().keys(), now).await.is_err(), "startup rejects a record beyond the retained state head"); }
                        }
                        else {
                            let error = result.expect_err(case);
                            let history::Error::Query(query) = &error else { panic!("{case}: {error:?}"); };
                            let expected = match case { "immutable assignment" => SqlState::INSUFFICIENT_PRIVILEGE, "state only" | "missing completion" => SqlState::FOREIGN_KEY_VIOLATION, "duplicate" => SqlState::UNIQUE_VIOLATION, _ => SqlState::CHECK_VIOLATION };
                            assert_eq!(query.0.code(), Some(&expected), "{entry:?}: {case}: {error:?}");
                        }
                        writer.client().batch_execute("ROLLBACK").await.expect("discard isolated structure case");
                        writer.terminate().await.expect("writer ended");
                        let row = observer.client().query_one("SELECT person.sequence,member.assignment_id,member.role_id,(SELECT count(*) FROM access.assignments WHERE grant_id=$1) AS count FROM access.grants member JOIN identity.persons person ON person.id=member.person_id WHERE member.id=$1 AND member.termination IS NULL", &[&grant.id()]).await.expect("independent complete graph");
                        assert_eq!(row.get::<_, i64>("sequence"), 1, "{case}");
                        assert_eq!(row.get::<_, i64>("count"), 1, "{case}");
                        assert_eq!(row.get::<_, super::Id>("assignment_id"), first.id(), "{case}");
                        assert_eq!(row.get::<_, role::Id>("role_id"), first.role().id(), "{case}");
                    }
                    Box::pin(services.assignment().assign(manager.clone(), operation(), command(&input(&candidate, &selected, entry, mobile)))).await.expect("real command jointly commits the state, record and completion");
                    crate::history::inspect::<crate::access::grant::state::State>(observer.client(), services.app().keys(), context.sample().await.expect("verification time")).await.expect("complete retained history");
                    let key_id = crate::keys::Id::new(1).expect("different key");
                    let missing = crate::keys::Keys::new(key_id, vec![crate::keys::Entry::new(key_id, crate::keys::Material(zeroize::Zeroizing::new([91;32])))]).expect("different retained inventory");
                    let keys = crate::history::Keys { data: &missing.encryption, seals: &missing.seal };
                    assert!(crate::kernel::completion::fetch_all::<std::convert::Infallible>(observer.client(), keys, super::operation::COMPLETIONS).await.is_err(), "permanent assignments prevent dropping a referenced completion key");
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("explicit damage confined to this disposable database");
                    owner.client().execute("DELETE FROM access.assignments WHERE id=$1", &[&first.id()]).await.expect("remove the required retained predecessor");
                    owner.client().batch_execute("COMMIT").await.expect("damage persisted");
                    assert!(services.grant().verify(observer.client()).await.is_err(), "startup detects missing permanent history");
                    services.assignment().candidate(&manager, entry, mobile.parse().expect("mobile"), pagination()).await.expect("current state does not consume a missing historical predecessor");
                    services.ready().await.expect("current state remains trustworthy");
                    owner.terminate().await.expect("owner ended");
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("runtime SQL scenario deadline");
            }
        }));
    }

    #[test]
    fn institutional_replacement_preserves_original_admission_and_each_completed_assignment() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(50), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), pagination()).await.expect("admission candidate");
                let default = candidate.roles().iter().find(|role| role.kind() == role::Kind::Default).expect("default");
                let subject = candidate.identity();
                let admission = json!({"entry":"institution","target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":"13800000000","assignmentId":candidate.assignment_id(),"suspensionId":candidate.suspension_id(),"grant":candidate.grant()},"role":{"id":default.id(),"changeId":default.change_id()}});
                let operation = operation();
                let (grant, first) = Box::pin(services.grant().create(manager.clone(), operation, serde_json::from_value(admission.clone()).expect("admission"))).await.expect("real admission");
                let selected = services.role().create_workforce(manager.clone(), self::operation(), serde_json::from_value(json!({"entry":"institution","name":"Badge Editors","level":600,"permissions":["workforce.roles"]})).expect("role input")).await.expect("custom role");
                let candidate = services.assignment().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), pagination()).await.expect("member candidate");
                assert_eq!(candidate.grant_id(), grant.id()); assert_eq!(candidate.assignment_id(), first.id());
                assert_eq!(candidate.count(), 3); assert_eq!(candidate.role().id(), first.role().id());
                let body = input(&candidate, &selected, Entry::Institution, "13800000000");
                let assigned = Box::pin(services.assignment().assign(manager.clone(), operation, command(&body))).await.expect("same UUID in an independent family");
                assert_eq!(assigned.grant_id(), grant.id()); assert_eq!(assigned.sequence(), 2);
                assert_eq!(assigned.role().id(), selected.id());
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.assignments WHERE grant_id=$1)=2 AND (SELECT count(*) FROM access.grants WHERE person_id=$2)=1 AND EXISTS(SELECT 1 FROM access.grants member JOIN access.grants admission ON admission.id=member.id JOIN identity.persons person ON person.id=member.person_id WHERE member.id=$1 AND member.termination IS NULL AND member.assignment_id=$3 AND member.role_id=$4 AND member.sequence=2 AND member.version=2 AND person.sequence=1) AND EXISTS(SELECT 1 FROM access.assignments WHERE id=$3 AND entry='institution' AND octet_length(seal)=32 AND seal_key_id IS NOT NULL)", &[&grant.id(), &candidate.identity().person_id(), &assigned.id(), &selected.id()]).await.expect("complete independent graph").get::<_, bool>(0));
                assert_eq!(services.grant().read_role(&member).await.expect("live role").id(), selected.id());
                let (_, repeated_first) = Box::pin(services.grant().create(manager.clone(), operation, serde_json::from_value(admission).expect("original admission"))).await.expect("first result survives replacement");
                assert_eq!(repeated_first.id(), first.id()); assert_eq!(repeated_first.role().id(), first.role().id());
                assert!(matches!(Box::pin(services.assignment().assign(manager.clone(), self::operation(), command(&body))).await, Err(Error::TargetChanged)));
                let fresh = services.assignment().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), pagination()).await.expect("fresh confirmation");
                assert!(matches!(Box::pin(services.assignment().assign(manager.clone(), self::operation(), command(&input(&fresh, &selected, Entry::Institution, "13800000000")))).await, Err(Error::Unchanged)));
                let full = fresh.roles().iter().find(|role| role.kind() == role::Kind::Full).expect("institution may select administrator");
                Box::pin(services.assignment().assign(manager.clone(), self::operation(), command(&input(&fresh, full, Entry::Institution, "13800000000")))).await.expect("replace with administrator");
                let replay = Box::pin(services.assignment().assign(manager.clone(), operation, command(&body))).await.expect("fixed original after another replacement");
                assert_eq!(replay.id(), assigned.id()); assert_eq!(replay.assigned_at(), assigned.assigned_at());
                assert_eq!(replay.role().id(), selected.id());
                let mut changed = body.clone(); changed["target"]["mobile"] = json!("13900000000");
                assert!(matches!(Box::pin(services.assignment().assign(manager.clone(), operation, command(&changed))).await, Err(Error::Mismatch)));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let replay = Box::pin(services.assignment().assign(manager, operation, command(&body))).await.expect("original after restart");
                assert_eq!(replay.id(), assigned.id());
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("replacement lifecycle deadline");
        }));
    }
}
