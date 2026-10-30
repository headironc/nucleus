pub(super) mod database;
mod operation;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, timeout_at};
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;

pub(crate) use crate::access::appointment::database::find_by_account;
pub(in crate::access) use crate::access::appointment::operation::COMPLETIONS;
pub use crate::access::appointment::operation::{CommandError, Input, Rejection, Target};
use crate::{
    access::role,
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
#[schema(as = appointment::Id, value_type = Uuid)]
pub struct Id(Uuid);

impl Id {
    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

pub struct Candidate {
    identity: account::Subject,
    assignment_id: Id,
    role: role::Role,
}

impl Candidate {
    pub fn identity(&self) -> &account::Subject {
        &self.identity
    }
    pub fn assignment_id(&self) -> Id {
        self.assignment_id
    }
    pub fn role(&self) -> &role::Role {
        &self.role
    }
}

pub struct Appointment {
    id: Id,
    subject: revision::Identity,
    operator: revision::Identity,
    role: role::Role,
    sequence: i64,
    assigned_at: DateTime<Utc>,
    operation_id: identity::operation::Id,
}

impl Appointment {
    pub fn id(&self) -> Id {
        self.id
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

impl Filter {
    pub fn new(
        pagination: Pagination,
        person_id: Option<person::Id>,
        operation_id: Option<identity::operation::Id>,
    ) -> Self {
        Self {
            pagination,
            person_id,
            operation_id,
        }
    }
}

pub struct List {
    count: i64,
    appointments: Vec<Appointment>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn appointments(&self) -> &[Appointment] {
        &self.appointments
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

struct Read {
    lease: platform::database::Lease,
    deadline: Instant,
}

pub(crate) async fn verify(
    client: &tokio_postgres::Client,
    app: &App,
    roles: &role::Reader,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<(), CommandError> {
    crate::history::start::<super::holder::State>(client, app.keys(), app.anchors(), now).await?;
    database::verify(client, roles, revisions, now).await
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

    pub async fn candidate(&self, actor: &Actor, mobile: Mobile) -> Result<Candidate, Error> {
        let read = self.begin().await?;
        let result = timeout_at(read.deadline, async {
            let client = read.lease.client();
            self.context
                .read_budget()
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    let mobile = &mobile;
                    async move {
                        let (role, person_id, _, now) =
                            self.authorize(client, &roles, actor).await?;
                        let id = self
                            .accounts
                            .at(view)
                            .find_by_mobile(client, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        let target = database::find_by_account(client, &roles, id, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        if target.person_id == person_id || target.role.level() >= role.level() {
                            return Err(Error::Absent);
                        }
                        let identity = self
                            .accounts
                            .at(view)
                            .find_by_id(client, id, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        Ok(Candidate {
                            identity,
                            assignment_id: target.id,
                            role: target.role,
                        })
                    }
                })
                .await
        })
        .await;
        self.finish(
            read,
            result.map_err(Error::from).and_then(std::convert::identity),
        )
        .await
    }

    pub async fn roles(&self, actor: &Actor, pagination: Pagination) -> Result<role::List, Error> {
        let read = self.begin().await?;
        let result = timeout_at(read.deadline, async {
            let client = read.lease.client();
            self.context
                .read_budget()
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    async move {
                        let (role, _, _, now) = self.authorize(client, &roles, actor).await?;
                        role::database::fetch_assignable(
                            client,
                            &roles,
                            pagination,
                            crate::access::capability::Scope::Institution,
                            &role,
                            now,
                        )
                        .await
                        .map_err(Error::from)
                    }
                })
                .await
        })
        .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => Err(error.into()),
        };
        self.finish(read, result).await
    }

    pub async fn list(&self, actor: &Actor, filter: Filter) -> Result<List, Error> {
        let read = self.begin().await?;
        let result = timeout_at(read.deadline, async {
            let client = read.lease.client();
            self.context
                .read_budget()
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    let filter = &filter;
                    async move {
                        let (_, _, _, now) = self.authorize(client, &roles, actor).await?;
                        database::fetch_list(client, &roles, filter, &self.revisions, now).await
                    }
                })
                .await
        })
        .await;
        self.finish(
            read,
            result.map_err(Error::from).and_then(std::convert::identity),
        )
        .await
    }

    async fn authorize(
        &self,
        client: &tokio_postgres::Client,
        roles: &role::Reader,
        actor: &Actor,
    ) -> Result<(role::Role, person::Id, revision::Id, DateTime<Utc>), Error> {
        Ok(crate::access::authorize(
            client,
            roles,
            &self.current,
            actor,
            role::Code::InstitutionAssignments,
        )
        .await?)
    }

    async fn rollback(lease: platform::database::Lease, cause: Error, deadline: Instant) -> Error {
        let (known, cleanup) = crate::database::transaction::rollback(
            cause,
            deadline,
            lease.client().batch_execute("ROLLBACK"),
        )
        .await
        .recycle(lease)
        .await;
        match known.end {
            Ok(()) => known.value,
            Err(rollback) => Error::Rollback {
                cause: Box::new(known.value),
                rollback,
            },
        }
        .combine(cleanup)
    }

    async fn begin(&self) -> Result<Read, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        Ok(Read { lease, deadline })
    }

    async fn finish<T>(&self, read: Read, result: Result<T, Error>) -> Result<T, Error> {
        if let Err(cause) = &result
            && runtime::work::Failure::unreliable(cause)
        {
            self.registry.close();
        }
        match result {
            Ok(value) => {
                match timeout_at(read.deadline, read.lease.client().batch_execute("COMMIT")).await {
                    Ok(Ok(())) => {
                        read.lease.release();
                        Ok(value)
                    }
                    Ok(Err(error)) => Err(Error::from(error).combine(read.lease.discard().await)),
                    Err(error) => Err(Error::Timeout(error).combine(read.lease.discard().await)),
                }
            }
            Err(cause) if cause.rejected() => {
                Err(Self::rollback(read.lease, cause, read.deadline).await)
            }
            Err(cause) => Err(cause.combine(read.lease.discard().await)),
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
    #[error("institutional appointment access is denied")]
    Denied,
    #[error("appointment input differs from the original operation")]
    Mismatch,
    #[error("the target is unavailable")]
    Absent,
    #[error("the confirmed target has changed")]
    TargetChanged,
    #[error("the selected role has changed")]
    RoleChanged,
    #[error("the target already holds the selected role")]
    Unchanged,
    #[error("{0}")]
    Current(#[from] current::Error),
    #[error("{0}")]
    Role(#[from] role::Error),
    #[error("{0}")]
    Account(#[from] account::ReadError),
    #[error("{0}")]
    Revision(#[from] revision::ReadError),
    #[error("appointment fact check failed: {rule}")]
    Facts { rule: &'static str },
    #[error("appointment query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("{0}")]
    Work(#[from] runtime::work::Error),
    #[error("appointment input or coordination encoding failed")]
    Crypto(#[from] platform::cryptography::Error),
    #[error("{0}")]
    Key(#[from] keys::request::Missing),
    #[error("appointment transaction end failed: {0}")]
    End(#[source] crate::database::transaction::EndError),
    #[error("original appointment did not commit")]
    NotCommitted {
        #[source]
        cause: Box<Error>,
        commit: Result<(), crate::database::transaction::EndError>,
        termination: Result<(), platform::database::Error>,
    },
    #[error("original appointment outcome remains unknown")]
    Unknown {
        #[source]
        cause: Box<Error>,
        recovery: Box<Error>,
    },
    #[error("appointment budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("appointment rejection rollback failed")]
    Rollback {
        #[source]
        cause: Box<Error>,
        rollback: crate::database::transaction::EndError,
    },
    #[error("appointment and connection termination failed")]
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
impl From<crate::database::transaction::Error> for Error {
    fn from(error: crate::database::transaction::Error) -> Self {
        match error {
            crate::database::transaction::Error::Query(error) => Self::Query(error),
            crate::database::transaction::Error::Crypto(error) => Self::Crypto(error),
            // The fence already closed admission; the outcome stays with the next process.
            crate::database::transaction::Error::Superseded => {
                Self::Work(runtime::work::Error::Closed)
            }
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
        ) || matches!(
            self,
            Self::Role(role::Error::Denied | role::Error::Current(current::Error::Invalid))
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
            Self::Facts { .. } | Self::Key(_) => true,
            Self::Current(error) => error.unreliable(),
            Self::Role(error) => error.unreliable(),
            Self::Account(error) => error.unreliable(),
            Self::Revision(error) => error.unreliable(),
            Self::Combined { cause, .. }
            | Self::Rollback { cause, .. }
            | Self::NotCommitted { cause, .. } => cause.unreliable(),
            Self::Unknown { cause, recovery } => cause.unreliable() || recovery.unreliable(),
            _ => false,
        }
    }
    fn report(&self, operation_id: identity::operation::Id) {
        if !self.rejected() {
            tracing::error!(%operation_id, stage = "appointment_completion", kind = self.failure().as_ref(), error = ?self);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::{
            appointment::{Candidate, CommandError, Error, Input, operation::Rejection},
            role::{
                self,
                tests::{actor, start},
            },
        },
        identity::{
            authentication::{
                Database, Supplier, server,
                session::{
                    Receipt,
                    tests::{CODE, input, message},
                },
            },
            current, operation,
            person::revision,
        },
        pagination::Pagination,
    };

    pub(crate) fn command(candidate: &Candidate, selected: &role::Role) -> Input {
        serde_json::from_value(json!({"target":{"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000","assignmentId":candidate.assignment_id()},"role":{"id":selected.id(),"changeId":selected.change_id()}})).expect("typed appointment")
    }

    pub(crate) fn mobile(index: u16) -> String {
        format!("13800000{index:03}")
    }

    /// Registers a distinct real person through R1 and R2 for history-scale scenarios.
    pub(crate) async fn register_numbered(
        database: &Database,
        services: &crate::startup::Services,
        supplier: &mut Supplier,
        index: u16,
    ) -> current::Actor {
        let mobile = mobile(index);
        let id_number = "0123456789X"
            .chars()
            .find_map(|suffix| {
                format!("00000020000229{index:03}{suffix}")
                    .parse::<crate::identity::person::number::IdNumber>()
                    .ok()
            })
            .expect("valid distinct fixture number");
        let id = message(database, services.authentication(), supplier, &mobile).await;
        let Receipt::Registration(credential) = Box::pin(
            services
                .authentication()
                .create_session(input(Uuid::new_v4(), id, &mobile, CODE)),
        )
        .await
        .expect("R1 registration credential") else {
            panic!("registration credential");
        };
        let registered = services
            .registration()
            .register(crate::registration::Input::new(
                Uuid::new_v4().to_string().parse().expect("operation"),
                credential.id(),
                credential
                    .token()
                    .text()
                    .parse()
                    .expect("registration proof"),
                "History Person".parse().expect("name"),
                id_number,
            ))
            .await
            .expect("real distinct R2 account");
        services
            .current()
            .authenticate(current::Proof {
                id: registered.id(),
                token: registered.token().text().parse().expect("session proof"),
            })
            .await
            .expect("registered actor")
    }

    pub(crate) async fn register(
        database: &Database,
        services: &crate::startup::Services,
        supplier: &mut Supplier,
    ) -> current::Actor {
        let id = message(database, services.authentication(), supplier, "13800000000").await;
        let Receipt::Registration(credential) =
            Box::pin(services.authentication().create_session(input(
                Uuid::new_v4(),
                id,
                "13800000000",
                CODE,
            )))
            .await
            .expect("R1")
        else {
            panic!("registration credential");
        };
        let registered = services
            .registration()
            .register(crate::registration::Input::new(
                Uuid::new_v4().to_string().parse().expect("operation"),
                credential.id(),
                credential.token().text().parse().expect("proof"),
                "Target Person".parse().expect("name"),
                "00000020000229007X".parse().expect("number"),
            ))
            .await
            .expect("R2 account");
        services
            .current()
            .authenticate(current::Proof {
                id: registered.id(),
                token: registered.token().text().parse().expect("session proof"),
            })
            .await
            .expect("target actor")
    }

    #[test]
    fn candidate_damage_closes_work_but_query_failures_and_hidden_targets_do_not() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
        for case in ["person", "revision", "envelope", "ciphertext", "digest", "query"] {
            timeout(Duration::from_secs(15), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                register(&database, &services, &mut supplier).await;
                let selected = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(json!({"name":"Selectable","level":20,"permissions":[]})).expect("role"))).await.expect("role");
                assert!(matches!(services.appointment().candidate(&manager, "13700000000".parse().expect("unknown mobile")).await, Err(Error::Absent)));
                let candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("healthy candidate");
                let owner = database.connections("postgres").recover().await.expect("isolated damage injector");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("explicit post-startup integrity fault");
                match case {
                    "person" => { owner.client().execute("DELETE FROM identity.persons WHERE id=$1", &[&candidate.identity().person_id()]).await.expect("missing necessary person"); }
                    "revision" => { owner.client().execute("UPDATE identity.persons SET revision_id=gen_random_uuid() WHERE id=$1", &[&candidate.identity().person_id()]).await.expect("missing necessary revision"); }
                    "envelope" => { owner.client().execute("UPDATE identity.revisions SET id_number=set_byte(id_number,0,255) WHERE id=$1", &[&candidate.identity().revision_id()]).await.expect("invalid envelope version"); }
                    "ciphertext" => { owner.client().execute("UPDATE identity.revisions SET id_number=set_byte(id_number,54,get_byte(id_number,54)#1) WHERE id=$1", &[&candidate.identity().revision_id()]).await.expect("failed authentication while name is valid"); }
                    "digest" => { owner.client().execute("UPDATE identity.accounts SET id_number=decode(repeat('09',32),'hex') WHERE id=$1", &[&candidate.identity().id()]).await.expect("wrong identity digest"); }
                    "query" => { owner.client().batch_execute(&format!("REVOKE SELECT ON identity.revisions FROM {}", database.user)).await.expect("ordinary query denial"); }
                    _ => unreachable!(),
                }
                owner.client().batch_execute("COMMIT").await.expect("isolated fault established");
                let error = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.err().expect("damaged candidate is not disclosed");
                assert_eq!(error.failure(), crate::access::appointment::Failure::Unavailable, "{case}: {error:?}");
                assert_eq!(crate::runtime::work::Failure::unreliable(&error), case != "query", "{case}: {error:?}");
                assert_eq!(owner.client().query_one("SELECT count(*) FROM access.appointments WHERE person_id=$1", &[&candidate.identity().person_id()]).await.expect("read has no business effects").get::<_,i64>(0), 1);
                if case == "query" {
                    owner.client().batch_execute(&format!("GRANT SELECT ON identity.revisions TO {}", database.user)).await.expect("restore disposable permission");
                    Box::pin(services.appointment().assign(manager, Uuid::new_v4().to_string().parse().expect("operation"), command(&candidate,&selected))).await.expect("ordinary query failure did not close new work");
                } else {
                    assert!(matches!(Box::pin(services.appointment().assign(manager, Uuid::new_v4().to_string().parse().expect("operation"), command(&candidate,&selected))).await, Err(error) if error.is_closed()), "fact failure closes later admission");
                }
                owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no extra external calls during candidate errors and drain"); supplier.stop().await;
            })).await.expect("damage scenario budget");
        }
    }));
    }

    #[test]
    fn runtime_sql_protects_holder_structure_and_commands_lock_the_target() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                register(&database, &services, &mut supplier).await;
                let selected = Box::pin(services.role().create(
                    manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"),
                    serde_json::from_value(json!({"name":"SQL replacement","level":30,"permissions":[]})).expect("definition"),
                )).await.expect("role");
                let candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("target");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let actor = observer.client().query_one(
                    "SELECT person.id,person.revision_id,holder.assignment_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id JOIN access.holders holder ON holder.id=account.id WHERE session.id=$1",
                    &[&manager.session_id()],
                ).await.expect("actor facts");
                let actor_person: crate::identity::person::Id = actor.get("id");
                let actor_revision: crate::identity::person::revision::Id = actor.get("revision_id");
                let actor_assignment: super::Id = actor.get("assignment_id");
                // Fake seals exercise structural SQL protection; no forged row is committed.
                for case in [
                    "state-only", "stale-version", "sequence-gap", "head-time", "missing-completion",
                    "wrong-definition", "wrong-predecessor", "unknown-key", "missing-seal",
                    "first-source", "marker", "history-rewrite", "history-delete", "state-delete",
                    "completion-delete", "early-head",
                ] {
                    let writer = database.connections(&database.user).recover().await.expect("runtime writer");
                    writer.client().batch_execute("BEGIN").await.expect("transaction");
                    let id = super::Id::allocate();
                    let operation: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
                    let now = chrono::Utc::now();
                    let seal = if case == "missing-seal" { None } else { Some(vec![73_u8; 32]) };
                    let key = if case == "unknown-key" { 99999_i64 } else { 0 };
                    let sequence = if case == "sequence-gap" { 3_i64 } else { 2 };
                    let version = if case == "stale-version" { 1_i64 } else { 2 };
                    let previous = if case == "wrong-predecessor" { actor_assignment } else { candidate.assignment_id() };
                    let change = if case == "wrong-definition" { candidate.role().change_id() } else { selected.change_id() };
                    let source = if case == "first-source" { "registration" } else { "session" };
                    let result: Result<(), tokio_postgres::Error> = async {
                        match case {
                            "history-rewrite" => { writer.client().execute("UPDATE access.appointments SET operation_id=$1 WHERE id=$2", &[&operation, &candidate.assignment_id()]).await?; }
                            "history-delete" => { writer.client().execute("DELETE FROM access.appointments WHERE id=$1", &[&candidate.assignment_id()]).await?; }
                            "state-delete" => { writer.client().execute("DELETE FROM access.holders WHERE id=$1", &[&candidate.identity().id()]).await?; }
                            "completion-delete" => { writer.client().execute("DELETE FROM access.completions WHERE operation_id=$1", &[&operation]).await?; }
                            "marker" => { writer.client().execute("UPDATE access.appointments SET transaction_id=pg_current_xact_id() WHERE id=$1", &[&candidate.assignment_id()]).await?; }
                            "state-only" => { writer.client().execute("UPDATE access.holders SET sequence=sequence+1,version=version+1 WHERE id=$1", &[&candidate.identity().id()]).await?; }
                            _ => {
                                if case != "missing-completion" {
                                    writer.client().execute(
                                        "INSERT INTO access.completions(family,operation_id,action,target,input,subject,person_id,revision_id,created_at,seal,seal_key_id,seal_version) VALUES('institution_appointment',$1,'assign',$2,$3,'session',$4,$5,$6,$7,0,1)",
                                        &[&operation, &candidate.identity().id().to_string(), &vec![8_u8;37], &actor_person, &actor_revision, &now, &vec![73_u8;32]],
                                    ).await?;
                                }
                                writer.client().execute(
                                    "INSERT INTO access.appointments(id,holder_id,person_id,role_id,previous_id,change_id,operator_id,revision_id,assigned_at,sequence,operation_id,source,seal,seal_key_id,seal_version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,1)",
                                    &[&id, &candidate.identity().id(), &candidate.identity().person_id(), &selected.id(), &previous, &change, &actor_revision, &candidate.identity().revision_id(), &now, &sequence, &operation, &source, &seal, &key],
                                ).await?;
                                let head_time = if case == "head-time" { now + chrono::TimeDelta::microseconds(1) } else { now };
                                writer.client().execute(
                                    "UPDATE access.holders SET role_id=$2,assignment_id=$3,changed_at=$4,sequence=$5,version=$6,seal=$7 WHERE id=$1",
                                    &[&candidate.identity().id(), &selected.id(), &id, &head_time, &sequence, &version, &vec![73_u8;32]],
                                ).await?;
                                if case == "early-head" {
                                    writer.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await?;
                                    writer.client().execute("UPDATE access.holders SET version=version+1 WHERE id=$1", &[&candidate.identity().id()]).await?;
                                }
                            }
                        }
                        writer.client().batch_execute("COMMIT").await
                    }.await;
                    let error = result.expect_err("incomplete structure or forbidden mutation");
                    use tokio_postgres::error::SqlState;
                    let expected = match case {
                        "history-rewrite" | "history-delete" | "state-delete" | "completion-delete" | "marker" => SqlState::INSUFFICIENT_PRIVILEGE,
                        "missing-completion" | "wrong-definition" | "wrong-predecessor" | "unknown-key" => SqlState::FOREIGN_KEY_VIOLATION,
                        "missing-seal" => SqlState::NOT_NULL_VIOLATION,
                        _ => SqlState::CHECK_VIOLATION,
                    };
                    assert_eq!(error.code(), Some(&expected), "{case}: {error:?}");
                    writer.client().batch_execute("ROLLBACK").await.expect("rollback");
                    writer.terminate().await.expect("writer ended");
                    assert!(observer.client().query_one(
                        "SELECT person.sequence=1 AND holder.sequence=1 AND holder.version=1 AND holder.assignment_id=$2 AND holder.role_id=$3 AND (SELECT count(*) FROM access.appointments WHERE holder_id=holder.id)=1 FROM identity.persons person JOIN access.holders holder ON holder.person_id=person.id WHERE person.id=$1",
                        &[&candidate.identity().person_id(), &candidate.assignment_id(), &candidate.role().id()],
                    ).await.expect("original graph after rejection").get::<_, bool>(0));
                }
                let gate = database.connections(&database.user).recover().await.expect("account gate");
                gate.client().batch_execute("BEGIN").await.expect("gate transaction");
                gate.client().execute("SELECT identity.lock_account($1)", &[&candidate.identity().id()]).await.expect("lock target");
                let blocker: i32 = gate.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("gate backend").get(0);
                let service = services.appointment().clone();
                let input = command(&candidate, &selected);
                let task = tokio::spawn(async move {
                    service.assign(manager, Uuid::new_v4().to_string().parse().expect("operation"), input).await
                });
                timeout(Duration::from_secs(3), async {
                    loop {
                        if observer.client().query_one(
                            "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_catalog.pg_blocking_pids(pid)))",
                            &[&blocker],
                        ).await.expect("actual account lock wait").get::<_, bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("command waits for the real target account");
                assert!(!task.is_finished());
                gate.client().batch_execute("COMMIT").await.expect("release target");
                gate.terminate().await.expect("gate ended");
                let assignment = timeout(Duration::from_secs(3), task).await.expect("bounded assignment").expect("task").expect("assignment");
                assert_eq!(observer.client().query_one("SELECT assignment_id FROM access.holders WHERE id=$1", &[&candidate.identity().id()]).await.expect("current head").get::<_, super::Id>(0), assignment.id());
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
            })).await.expect("SQL and lock deadline");
        }));
    }
    #[test]
    fn concurrent_replacements_recheck_confirmation_and_opposite_accounts_cannot_deadlock() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(25),Box::pin(async {
            let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;
            let (context,services)=start(database.connections(&database.user),&supplier).await;
            let manager=actor(&database,&services,&mut supplier).await;let target=register(&database,&services,&mut supplier).await;
            let selected=Box::pin(services.role().create(manager.clone(),Uuid::new_v4().to_string().parse().expect("role operation"),serde_json::from_value(json!({"name":"Delegated assignment","level":300,"permissions":["institution.assignments"]})).expect("definition"))).await.expect("role");
            let candidate=services.appointment().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");
            let observer=database.connections(&database.user).recover().await.expect("observer");
            let gate=database.connections("postgres").recover().await.expect("controlled holder lock");
            gate.client().batch_execute("BEGIN").await.expect("gate");gate.client().execute("SELECT id FROM access.holders WHERE id=$1 FOR UPDATE",&[&candidate.identity().id()]).await.expect("hold target holder");
            let gate_pid:i32=gate.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("holder gate PID").get(0);
            let holder_lock=format!("{} WHERE state.id = $1 FOR UPDATE OF state",<crate::access::holder::State as crate::history::Kind>::SELECT);
            let spawn=|input|{let service=services.appointment().clone();let manager=manager.clone();tokio::spawn(async move {service.assign(manager,Uuid::new_v4().to_string().parse().expect("operation"),input).await})};
            let first=spawn(command(&candidate,&selected));let second=spawn(command(&candidate,&selected));
            timeout(Duration::from_secs(3),async {loop {
                let row=observer.client().query_one("WITH RECURSIVE blocked(pid) AS (SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_catalog.pg_blocking_pids(pid)) UNION SELECT backend.pid FROM pg_catalog.pg_stat_activity backend JOIN blocked ON blocked.pid=ANY(pg_catalog.pg_blocking_pids(backend.pid)) WHERE backend.datname=current_database()) SELECT count(*) FILTER(WHERE query=$2),count(*) FILTER(WHERE query='SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE') FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename=current_user AND pid IN(SELECT pid FROM blocked) AND wait_event_type='Lock'",&[&gate_pid,&holder_lock]).await.expect("both actual accepted writes are queued behind the holder gate");
                if row.get::<_,i64>(0)==1 && row.get::<_,i64>(1)==1 {break;}tokio::task::yield_now().await;
            }}).await.expect("same-target account/holder lock serialization");
            gate.client().batch_execute("COMMIT").await.expect("release target");gate.terminate().await.expect("gate ended");
            let (first,second)=timeout(Duration::from_secs(3),async{(first.await.expect("first work"),second.await.expect("second work"))}).await.expect("both complete");
            assert_eq!(usize::from(first.is_ok())+usize::from(second.is_ok()),1);
            let error=if let Err(error)=first {error}else{second.err().expect("one old confirmation rejected")};assert!(matches!(error,CommandError::Rejected(Rejection::TargetChanged)),"{error:?}");
            assert_eq!(observer.client().query_one("SELECT count(*) FROM access.appointments WHERE person_id=$1",&[&candidate.identity().person_id()]).await.expect("only one new immutable appointment before more work").get::<_,i64>(0),2);

            let next=services.appointment().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("new confirmation");
            let actor_row=observer.client().query_one("SELECT account.id,person.id AS person_id,person.revision_id,holder.assignment_id,holder.role_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id JOIN access.holders holder ON holder.id=account.id WHERE session.id=$1",&[&manager.session_id()]).await.expect("manager facts for deliberately forbidden opposite target");
            let reverse:Input=serde_json::from_value(json!({"target":{"accountId":actor_row.get::<_,Uuid>("id"),"personId":actor_row.get::<_,Uuid>("person_id"),"revisionId":actor_row.get::<_,Uuid>("revision_id"),"assignmentId":actor_row.get::<_,Uuid>("assignment_id"),"mobile":"13900000000"},"role":{"id":candidate.role().id(),"changeId":candidate.role().change_id()}})).expect("typed reverse confirmation");
            let gate=database.connections(&database.user).recover().await.expect("fixed account gate");gate.client().batch_execute("BEGIN").await.expect("gate transaction");
            let lower=if actor_row.get::<_,Uuid>("id")<serde_json::from_value::<Uuid>(serde_json::to_value(candidate.identity().id()).expect("target ID")).expect("UUID") {actor_row.get::<_,Uuid>("id")}else{serde_json::from_value::<Uuid>(serde_json::to_value(candidate.identity().id()).expect("target ID")).expect("UUID")};
            gate.client().execute("SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE",&[&lower]).await.expect("hold smallest account exclusively");
            let gate_pid:i32=gate.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("account gate PID").get(0);
            let first=spawn(command(&next,candidate.role()));
            let service=services.appointment().clone();let current=target.clone();let second=tokio::spawn(async move{service.assign(current,Uuid::new_v4().to_string().parse().expect("reverse operation"),reverse).await});
            timeout(Duration::from_secs(3),async {loop {
                let row=observer.client().query_one("WITH RECURSIVE blocked(pid) AS (SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_catalog.pg_blocking_pids(pid)) UNION SELECT backend.pid FROM pg_catalog.pg_stat_activity backend JOIN blocked ON blocked.pid=ANY(pg_catalog.pg_blocking_pids(backend.pid)) WHERE backend.datname=current_database()) SELECT count(*) FILTER(WHERE query='SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE'),count(*) FILTER(WHERE query='SELECT id FROM identity.accounts WHERE id=$1 FOR SHARE') FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename=current_user AND pid IN(SELECT pid FROM blocked) AND wait_event_type='Lock'",&[&gate_pid]).await.expect("both directions wait on the exact smallest-account lock modes");
                if row.get::<_,i64>(0)==1 && row.get::<_,i64>(1)==1 {break;}tokio::task::yield_now().await;
            }}).await.expect("opposite requests acquired no later account out of order");
            gate.client().batch_execute("COMMIT").await.expect("release smallest account");gate.terminate().await.expect("gate ended");
            let (first,second)=timeout(Duration::from_secs(3),async{(first.await.expect("forward work"),second.await.expect("reverse work"))}).await.expect("no deadlock");
            first.expect("authorized forward replacement");assert!(matches!(second,Err(CommandError::Rejected(Rejection::Absent))|Err(CommandError::Denied)),"{:?}",second.as_ref().err());
            assert_eq!(observer.client().query_one("SELECT count(*) FROM access.appointments WHERE person_id=$1",&[&candidate.identity().person_id()]).await.expect("exact serial effects").get::<_,i64>(0),3);
            observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
        })).await.expect("concurrent replacement budget");
    }));
    }

    #[test]
    fn person_records_exclude_other_objects_and_startup_rejects_damaged_history() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
        for case in ["drifted sequence", "missing registration", "extra event", "empty latest event", "merged event"] {
            timeout(Duration::from_secs(30),Box::pin(async {
                let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let manager=actor(&database,&services,&mut supplier).await;register(&database,&services,&mut supplier).await;
                let selected=Box::pin(services.role().create(manager.clone(),Uuid::new_v4().to_string().parse().expect("role operation"),serde_json::from_value(json!({"name":"Event index","level":25,"permissions":[]})).expect("definition"))).await.expect("role");
                let candidate=services.appointment().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");
                let original=Box::pin(services.appointment().assign(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),command(&candidate,&selected))).await.expect("real appointment");
                let person=candidate.identity().person_id();
                let row=database.connections(&database.user).recover().await.expect("observer");
                let separate=row.client().query_one(
                    "SELECT holder.sequence,appointment.sequence,person.sequence,account.sequence
                     FROM access.holders holder
                     JOIN access.appointments appointment ON appointment.id=holder.assignment_id
                     JOIN identity.persons person ON person.id=holder.person_id
                     JOIN identity.accounts account ON account.id=holder.id
                     WHERE holder.person_id=$1 AND appointment.id=$2",
                    &[&person,&original.id()],
                ).await.expect("independent Holder sequence");
                assert_eq!(original.sequence(),2);
                assert_eq!(separate.get::<_,i64>(0),2);
                assert_eq!(separate.get::<_,i64>(1),2);
                assert_eq!(separate.get::<_,i64>(2),1,"appointment does not advance the person object");
                assert_eq!(separate.get::<_,i64>(3),1,"Account starts at its own creation sequence");

                let account=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("account target");
                let input=serde_json::from_value(json!({"personId":person,"revisionId":account.identity().revision_id(),"mobile":"13800000000"})).expect("suspension input");
                let paused=Box::pin(services.suspension().suspend(manager.clone(),Uuid::new_v4().to_string().parse().expect("suspension operation"),account.identity().id(),input)).await.expect("real suspension");
                assert!(row.client().query_one("SELECT sequence=2 AND suspension_id=$2 FROM identity.accounts WHERE id=$1",&[&account.identity().id(),&paused.id()]).await.expect("Account pause advances its own state").get::<_,bool>(0));
                let input=serde_json::from_value(json!({"personId":person,"revisionId":account.identity().revision_id(),"mobile":"13800000000","suspensionId":paused.id()})).expect("resumption input");
                let resumed=Box::pin(services.suspension().resume(manager.clone(),Uuid::new_v4().to_string().parse().expect("resumption operation"),account.identity().id(),input)).await.expect("real resumption");
                assert_eq!(resumed.id(),paused.id());

                // Account transitions advance their sealed object without touching the Person.
                let registered=row.client().query_one(
                    "SELECT person.sequence,
                        EXISTS(SELECT 1 FROM identity.suspensions suspension
                            WHERE suspension.id=$2 AND suspension.person_id=person.id AND suspension.sequence=2),
                        EXISTS(SELECT 1 FROM identity.resumptions resumption
                            WHERE resumption.id=$2 AND resumption.person_id=person.id AND resumption.sequence=3),
                        EXISTS(SELECT 1 FROM access.holders WHERE person_id=person.id AND sequence=2 AND assignment_id=$3),
                        EXISTS(SELECT 1 FROM identity.accounts WHERE person_id=person.id AND sequence=3 AND suspension_id IS NULL)
                     FROM identity.persons person WHERE person.id=$1",
                    &[&person,&paused.id(),&original.id()],
                ).await.expect("independent Account, Holder and Person state after Account transitions");
                assert_eq!(registered.get::<_,i64>(0),1,"Account and Holder transitions do not advance the Person");
                for field in 1..5 { assert!(registered.get::<_,bool>(field)); }

                let admission=|candidate:&crate::access::grant::Candidate,mobile:&str,kind:role::Kind| {
                    let subject=candidate.identity();
                    let role=candidate.roles().iter().find(|role|role.kind()==kind).expect("selected system role");
                    serde_json::from_value::<crate::access::grant::Input>(json!({"entry":"institution","target":{
                        "accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),
                        "mobile":mobile,"assignmentId":candidate.assignment_id(),"suspensionId":candidate.suspension_id(),"grant":candidate.grant()
                    },"role":{"id":role.id(),"changeId":role.change_id()}})).expect("complete admission confirmation")
                };
                let domain_manager=Box::pin(register_numbered(&database,&services,&mut supplier,42)).await;
                let domain_candidate=services.grant().candidate(&manager,crate::access::Entry::Institution,mobile(42).parse().expect("manager mobile"),Pagination::new(1,20).expect("pagination")).await.expect("domain manager candidate");
                Box::pin(services.grant().create(manager.clone(),Uuid::new_v4().to_string().parse().expect("manager admission"),admission(&domain_candidate,&mobile(42),role::Kind::Full))).await.expect("real domain authority");
                let grant_candidate=services.grant().candidate(&manager,crate::access::Entry::Institution,"13800000000".parse().expect("target mobile"),Pagination::new(1,20).expect("pagination")).await.expect("target admission candidate");
                let (grant,first)=Box::pin(services.grant().create(manager.clone(),Uuid::new_v4().to_string().parse().expect("target admission"),admission(&grant_candidate,"13800000000",role::Kind::Default))).await.expect("real independent Grant admission");
                assert_eq!((grant.sequence(),first.sequence()),(1,1));
                let grant_candidate=services.grant().suspension_candidate(&domain_manager,"13800000000".parse().expect("target mobile")).await.expect("domain pause candidate");
                let confirmation=json!({"target":{"accountId":grant_candidate.identity().id(),"personId":person,"revisionId":grant_candidate.identity().revision_id(),"mobile":"13800000000"}});
                let domain_paused=Box::pin(services.grant().suspend(domain_manager.clone(),Uuid::new_v4().to_string().parse().expect("domain suspension"),grant.id(),serde_json::from_value(confirmation.clone()).expect("pause confirmation"))).await.expect("real Grant pause");
                let domain_resumed=Box::pin(services.grant().resume(domain_manager,Uuid::new_v4().to_string().parse().expect("domain resumption"),domain_paused.id(),serde_json::from_value(confirmation).expect("resume confirmation"))).await.expect("real Grant resumption");
                assert_eq!((domain_paused.sequence(),domain_resumed.sequence()),(2,3));
                let indexed=row.client().query_one(
                    "SELECT person.sequence,
                        EXISTS(SELECT 1 FROM access.grants state JOIN access.assignments assignment ON assignment.grant_id=state.id
                            WHERE state.id=$3 AND state.person_id=person.id AND state.sequence=3 AND state.version=3 AND assignment.sequence=1),
                        EXISTS(SELECT 1 FROM access.suspensions suspension WHERE suspension.id=$2 AND suspension.person_id=person.id AND suspension.sequence=2),
                        EXISTS(SELECT 1 FROM access.resumptions resumption WHERE resumption.id=$2 AND resumption.person_id=person.id AND resumption.sequence=3)
                     FROM identity.persons person WHERE person.id=$1",
                    &[&person,&domain_paused.id(),&grant.id()],
                ).await.expect("Grant advances without joining the person object");
                assert_eq!(indexed.get::<_,i64>(0),1,"Grant transitions do not advance the Person");
                for field in 1..4 { assert!(indexed.get::<_,bool>(field)); }
                let correction_candidate=services.correction().candidate_by_person(&manager,person).await.expect("current identity confirmation");
                let corrected=Box::pin(services.correction().correct(manager.clone(),Uuid::new_v4().to_string().parse().expect("correction operation"),serde_json::from_value(json!({"target":correction_candidate.target(),"name":"Indexed Person","idNumber":"000000200002290029"})).expect("correction input"))).await.expect("real indexed identity correction");
                // The earlier Account suspension revoked the original session.
                let message_id=message(&database,services.authentication(),&mut supplier,"13800000000").await;
                let Receipt::Session(issued)=Box::pin(services.authentication().create_session(crate::identity::authentication::session::tests::input(Uuid::new_v4(),message_id,"13800000000",CODE))).await.expect("fresh target session") else {panic!("existing account");};
                let target=services.current().authenticate(current::Proof{id:issued.id(),token:issued.token().text().parse().expect("session proof")}).await.expect("fresh target actor");
                let allocated=Box::pin(services.number().allocate(target,Uuid::new_v4().to_string().parse().expect("allocation operation"),serde_json::from_value(json!({"subtype":801})).expect("allocation input"))).await.expect("independent number allocation");
                let personal=row.client().query_one("SELECT person.sequence,EXISTS(SELECT 1 FROM identity.corrections correction WHERE correction.id=$2 AND correction.person_id=person.id AND correction.sequence=2 AND correction.before_id=(SELECT revision_id FROM identity.origins WHERE person_id=person.id)),EXISTS(SELECT 1 FROM workforce.allocations allocation WHERE allocation.number_id=$3 AND allocation.person_id=person.id AND allocation.sequence=1) FROM identity.persons person WHERE person.id=$1",&[&person,&corrected.correction().id(),&allocated.id()]).await.expect("independent correction record");
                assert_eq!(personal.get::<_,i64>(0),2,"only the correction advances the Person");
                assert!(personal.get::<_,bool>(1));assert!(personal.get::<_,bool>(2));
                crate::identity::person::verify(
                    row.client(),
                    services.app().keys(),
                    row.client().query_one("SELECT clock_timestamp()", &[]).await.expect("trusted time").get(0),
                ).await.expect("valid person record family before damage");
                // Runtime writes append records; retained records cannot be changed or removed.
                for sql in [
                    "UPDATE identity.origins SET seal=set_byte(seal,0,0) WHERE person_id=$1",
                    "DELETE FROM identity.corrections WHERE id=$1",
                ] {
                    let error=row.client().execute(sql,&[&corrected.correction().id()]).await.expect_err("runtime person record mutation");
                    assert_eq!(error.code(),Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE));
                }
                row.terminate().await.expect("observer ended");
                let owner=database.connections("postgres").recover().await.expect("owner");
                if case == "merged event" {
                    Box::pin(services.cancellation().cancel_account(
                        manager.clone(), Uuid::new_v4().to_string().parse().expect("cancellation operation"),
                        candidate.identity().id(), serde_json::from_value(json!({
                            "personId": person, "revisionId": corrected.correction().id().revision_id(),
                            "mobile": "13800000000"
                        })).expect("current cancellation confirmation")
                    )).await.expect("cancellation advances the same person record family");
                }
                services.close();services.drain().await.expect("drained");context.stop().await.expect("context stopped");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("damage");
                let damage = match case {
                    "drifted sequence" => "UPDATE identity.corrections SET sequence=sequence+1000 WHERE id=$1",
                    "missing registration" => "DELETE FROM identity.corrections WHERE id=$1",
                    "extra event" => "UPDATE identity.persons SET sequence=sequence+1 WHERE id=(SELECT person_id FROM identity.corrections WHERE id=$1)",
                    "empty latest event" => "UPDATE identity.persons SET sequence=sequence-1 WHERE id=(SELECT person_id FROM identity.corrections WHERE id=$1)",
                    // The current revision advances without its new correction record.
                    _ => "UPDATE identity.persons SET revision_id=(SELECT before_id FROM identity.corrections WHERE id=$1) WHERE id=(SELECT person_id FROM identity.corrections WHERE id=$1)",
                };
                assert_eq!(owner.client().execute(damage,&[&corrected.correction().id()]).await.expect("record damage"),1,"{case}: exactly one selected record effect");
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                owner.terminate().await.expect("owner ended");
                let context=crate::runtime::Context::new(database.connections(&database.user)).await.expect("new clock");
                let result=Box::pin(crate::startup::Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                    crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                    crate::startup::Settings::new(4,crate::identity::message::Limits::default(),crate::identity::authentication::session_settings(),crate::registration::Limits::default(),crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(),crate::identity::authentication::unused_client().await,
                )).await;
                assert!(matches!(result,Err(crate::startup::Error::Identity(crate::identity::verification::Error::History(_)))),"{case}: {:?}",result.as_ref().err());
                context.stop().await.expect("context stopped");supplier.stop().await;
            })).await.expect("bounded Person history check");
        }
        }));
    }

    #[test]
    fn permanent_original_damage_is_not_absence_and_startup_checks_the_same_facts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
        for case in ["operator","predecessor","seal","key","time"] {
            timeout(Duration::from_secs(15),Box::pin(async {
                let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let manager=actor(&database,&services,&mut supplier).await;register(&database,&services,&mut supplier).await;
                let selected=Box::pin(services.role().create(manager.clone(),Uuid::new_v4().to_string().parse().expect("role operation"),serde_json::from_value(json!({"name":"Original integrity","level":25,"permissions":[]})).expect("definition"))).await.expect("role");
                let candidate=services.appointment().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");
                let operation=Uuid::new_v4().to_string().parse().expect("operation");
                let original=Box::pin(services.appointment().assign(manager.clone(),operation,command(&candidate,&selected))).await.expect("real complete original");
                let owner=database.connections("postgres").recover().await.expect("isolated integrity fault");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("post-startup privileged restore damage");
                match case {
                    "operator"=>{owner.client().execute("UPDATE access.appointments SET operator_id=gen_random_uuid() WHERE id=$1",&[&original.id()]).await.expect("missing original operator while current actor is healthy");}
                    "predecessor"=>{owner.client().execute("UPDATE access.appointments SET previous_id=gen_random_uuid() WHERE id=$1",&[&original.id()]).await.expect("damaged fixed predecessor");}
                    "seal"=>{owner.client().execute("UPDATE access.appointments SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE id=$1",&[&original.id()]).await.expect("damaged retained seal");}
                    "key"=>{owner.client().execute("UPDATE access.appointments SET seal_key_id=99999 WHERE id=$1",&[&original.id()]).await.expect("unknown retained sealing key");}
                    "time"=>{owner.client().execute("UPDATE access.appointments SET assigned_at=(SELECT changed_at FROM access.changes WHERE id=$2) WHERE id=$1",&[&original.id(),&selected.change_id()]).await.expect("changed authenticated appointment time");}
                    _=>unreachable!(),
                }
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                assert_eq!(owner.client().query_one("SELECT count(*) FROM access.appointments WHERE operation_id=$1 AND sequence>1",&[&operation]).await.expect("original definitely exists before replay").get::<_,i64>(0),1);
                let error=Box::pin(services.appointment().assign(manager,operation,command(&candidate,&selected))).await.err().expect("corrupt original never becomes successful or absent");
                let CommandError::Corrupt(fault)=&error else {panic!("{case}: {error:?}");};
                match case {
                    "key"=>assert!(matches!(fault.kind(),crate::kernel::FaultKind::History(crate::history::Error::Seal(crate::keys::seal::Error::Missing(id))) if *id==crate::keys::Id::new(99999).expect("injected missing key")),"{case}: {error:?}"),
                    _=>assert!(matches!(fault.kind(),crate::kernel::FaultKind::History(crate::history::Error::Seal(crate::keys::seal::Error::Mismatch))),"{case}: {error:?}"),
                }
                assert!(services.appointment().registry.ready().is_err());
                assert!(services.ready().await.is_err(),"damage closes common admission");
                assert_eq!(owner.client().query_one("SELECT count(*) FROM access.appointments WHERE operation_id=$1 AND sequence>1",&[&operation]).await.expect("no replacement or duplicated original").get::<_,i64>(0),1);
                services.close();services.drain().await.expect("drained");context.stop().await.expect("context stopped");
                let context=crate::runtime::Context::new(database.connections(&database.user)).await.expect("new clock");
                let result=Box::pin(crate::startup::Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                    crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                    crate::startup::Settings::new(4,crate::identity::message::Limits::default(),crate::identity::authentication::session_settings(),crate::registration::Limits::default(),crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(),crate::identity::authentication::unused_client().await,
                )).await;
                let Err(crate::startup::Error::AppointmentCommand(CommandError::Corrupt(fault)))=&result else {panic!("{case}: {:?}",result.as_ref().err());};
                match case {
                    "key"=>assert!(matches!(fault.kind(),crate::kernel::FaultKind::History(crate::history::Error::Seal(crate::keys::seal::Error::Missing(id))) if *id==crate::keys::Id::new(99999).expect("injected missing key")),"{case}: {fault:?}"),
                    _=>assert!(matches!(fault.kind(),crate::kernel::FaultKind::History(crate::history::Error::Seal(crate::keys::seal::Error::Mismatch))),"{case}: {fault:?}"),
                }
                context.stop().await.expect("rejected startup context ended");owner.terminate().await.expect("owner ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err(),"no external calls from damage, replay or startup");supplier.stop().await;
            })).await.expect("original damage case deadline");
        }
    }));
    }

    #[test]
    fn historical_pages_order_holder_sequence_independently_of_global_time() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(database.connections(&database.user), &supplier).await;
            let manager = actor(&database, &services, &mut supplier).await;
            register(&database, &services, &mut supplier).await;
            let selected = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("role operation"), serde_json::from_value(json!({"name":"Same-time history","level":25,"permissions":[]})).expect("definition"))).await.expect("role");
            let candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("target");
            let first = Box::pin(services.appointment().assign(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), command(&candidate, &selected))).await.expect("first replacement");
            let next = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("next target");
            let second = Box::pin(services.appointment().assign(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), command(&next, candidate.role()))).await.expect("second replacement");
            let high: crate::access::appointment::Id = serde_json::from_value(json!("ffffffff-ffff-4fff-8fff-ffffffffffff")).expect("high ID");
            let low: crate::access::appointment::Id = serde_json::from_value(json!("00000000-0000-4000-8000-000000000001")).expect("low ID");
            let owner = database.connections("postgres").recover().await.expect("isolated same-time material");
            // This reader fixture supplies authenticated history; it does not exercise command replay.
            owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated maintenance");
            let app = &services.appointment().app;
            let state = crate::access::holder::order_fixture(
                owner.client(), app.keys(), candidate.identity().id(), [first.id(), second.id()], [high, low],
            ).await.expect("paired sealed same-time history");
            owner.client().batch_execute("COMMIT").await.expect("same-time permanent fixture");
            crate::history::Seed::anchor(app.anchors(), &state).expect("install committed fixture anchor");
            let rows = owner.client().query("SELECT id,sequence,assigned_at FROM access.appointments WHERE person_id=$1 AND sequence>1 ORDER BY sequence DESC", &[&candidate.identity().person_id()]).await.expect("independent permanent order before read");
            assert_eq!(rows[0].get::<_,crate::access::appointment::Id>(0),low);
            assert_eq!(rows[1].get::<_,crate::access::appointment::Id>(0),high);
            assert_eq!(rows[0].get::<_,chrono::DateTime<chrono::Utc>>(2),rows[1].get::<_,chrono::DateTime<chrono::Utc>>(2));
            let filter = |person| crate::access::appointment::Filter::new(Pagination::new(1,100).expect("page"), person, None);
            let personal = services.appointment().list(&manager, filter(Some(candidate.identity().person_id()))).await.expect("personal sequence order");
            assert_eq!(personal.appointments().iter().map(|appointment|appointment.sequence()).collect::<Vec<_>>(),[3,2,1]);
            let global = services.appointment().list(&manager, filter(None)).await.expect("global time and stable ID order");
            assert_eq!(global.appointments()[0].id(),high); assert_eq!(global.appointments()[1].id(),low);
            owner.terminate().await.expect("maintenance ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
            assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
        }));
    }

    #[test]
    fn historical_reads_keep_authorization_count_projection_and_cleanup_in_one_budget() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = crate::access::role::tests::start_snapshot(crate::identity::authentication::proxied(&database,&proxy), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let target = register(&database, &services, &mut supplier).await;
                let selected = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("role operation"), serde_json::from_value(json!({"name":"History reader","level":300,"permissions":["institution.assignments"]})).expect("definition"))).await.expect("role");
                let candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("target");
                Box::pin(services.appointment().assign(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), command(&candidate, &selected))).await.expect("delegated reader");
                let filter = || crate::access::appointment::Filter::new(Pagination::new(1,100).expect("page"), None, None);
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                proxy.arm(Fault::StalledHolderStates);
                let service = services.appointment().clone(); let current = target.clone();
                let task = tokio::spawn(async move { service.list(&current, filter()).await });
                timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("holder states after authorization snapshot");
                Box::pin(services.role().update(manager.clone(), Uuid::new_v4().to_string().parse().expect("update operation"), selected.id(), role::Update::new(selected.change_id(), serde_json::from_value(json!({"name":"Renamed reader","level":300,"permissions":["institution.assignments"]})).expect("definition")))).await.expect("concurrent role definition");
                let next = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("new confirmation");
                let added = Box::pin(services.appointment().assign(manager.clone(), Uuid::new_v4().to_string().parse().expect("revoke operation"), command(&next, candidate.role()))).await.expect("concurrent permission loss");
                proxy.release();
                let error = timeout(Duration::from_secs(2), task).await.expect("changed snapshot is retried").expect("reader").err().expect("retry rechecks the lost assignment permission");
                assert!(matches!(error, Error::Role(role::Error::Denied)), "{error:?}");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.appointments", &[]).await.expect("four committed rows").get::<_,i64>(0),4);
                let latest = services.appointment().list(&manager, filter()).await.expect("fresh authorized snapshot");
                assert_eq!(latest.count(), 4); assert_eq!(latest.appointments()[0].id(), added.id());
                assert!(matches!(services.appointment().list(&target,filter()).await,Err(Error::Role(role::Error::Denied))));
                proxy.arm(Fault::RejectedRollback);
                let error = services.appointment().list(&target,filter()).await.err().expect("denied rollback failure");
                let cause = match &error { Error::Combined {cause,..} => cause.as_ref(), _ => &error };
                assert!(matches!(cause,Error::Rollback{cause,rollback:crate::database::transaction::EndError::Rollback(_)} if matches!(cause.as_ref(),Error::Role(role::Error::Denied))),"{error:?}");
                assert_eq!(services.appointment().list(&manager,filter()).await.expect("next complete snapshot").count(),4);
                // Deadline and finalization checks retain the production read budget.
                services.close(); services.drain().await.expect("snapshot services drained"); context.stop().await.expect("snapshot context stopped");
                let (context, services) = start(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                // Cancellation drops the owned read lease; it cannot return a dirty connection.
                proxy.arm(Fault::StalledRoleCount);
                let service = services.appointment().clone(); let current = manager.clone();
                let task = tokio::spawn(async move { service.list(&current,filter()).await });
                let pid=timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("cancelled history count");
                task.abort(); assert!(matches!(task.await, Err(error) if error.is_cancelled())); proxy.release();
                timeout(Duration::from_secs(3),async {loop {if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",&[&pid]).await.expect("cancelled backend ended").get::<_,bool>(0){break;}tokio::task::yield_now().await;}}).await.expect("read connection discarded");
                let started=tokio::time::Instant::now(); proxy.arm(Fault::StalledRoleCount);
                let service=services.appointment().clone();let current=manager.clone();
                let task=tokio::spawn(async move{service.list(&current,filter()).await});
                let pid=timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("first budget wait");
                tokio::time::sleep(Duration::from_millis(1500)).await; proxy.arm(Fault::StalledRolePage); proxy.release();
                timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("second page wait");
                let error=timeout(Duration::from_millis(2300),task).await.expect("one deadline").expect("reader").err().expect("incomplete page");
                assert!(matches!(error,Error::Timeout(_)),"{error:?}"); assert!(started.elapsed()<Duration::from_millis(3900)); proxy.release();
                timeout(Duration::from_secs(3),async {loop {if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",&[&pid]).await.expect("timed-out backend ended").get::<_,bool>(0){break;}tokio::task::yield_now().await;}}).await.expect("deadline discarded real connection");
                proxy.arm(Fault::StalledRoleCount);
                let service=services.appointment().clone();let current=manager.clone();let task=tokio::spawn(async move{service.list(&current,filter()).await});
                let pid=timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("count before read COMMIT");
                proxy.arm(Fault::LostReadCommit);proxy.release();
                let error=timeout(Duration::from_secs(3),task).await.expect("lost read commit").expect("reader").err().expect("read result withheld");
                let cause=match &error{Error::Combined{cause,..}=>cause.as_ref(),_=>&error};assert!(matches!(cause,Error::Query(_)),"{error:?}");
                proxy.release();timeout(Duration::from_secs(3),async {loop {if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",&[&pid]).await.expect("lost-commit backend ended").get::<_,bool>(0){break;}tokio::task::yield_now().await;}}).await.expect("read commit fault fully released");
                assert!(services.appointment().registry.ready().is_ok(),"ordinary read transport failure does not corrupt facts");
                assert_eq!(services.appointment().list(&manager,filter()).await.expect("new reader after failed commit").count(),4);
                let instance = context.clock_session().await;
                assert_eq!(services.appointment().list(&manager,filter()).await.expect("history reads without the instance session").count(),4);
                drop(instance);
                proxy.release();observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");let _=context.stop().await;
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err(),"no external requests through all read failures and drain");supplier.stop().await;proxy.stop().await;
            })).await.expect("history snapshot and resource deadline");
        }));
    }

    #[test]
    fn historical_pages_reject_required_fact_and_identity_damage_after_authorization() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["operator", "subject", "predecessor", "proof", "name", "id_number", "envelope", "definition", "first time"] {
                timeout(Duration::from_secs(15), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = register(&database, &services, &mut supplier).await;
                    let selected = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("role operation"), serde_json::from_value(json!({"name":"Historical integrity","level":25,"permissions":[]})).expect("definition"))).await.expect("role");
                    let candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("target");
                    let operation = Uuid::new_v4().to_string().parse().expect("operation");
                    let original = Box::pin(services.appointment().assign(manager.clone(), operation, command(&candidate, &selected))).await.expect("online appointment");
                    let changed = Box::pin(services.role().update(manager.clone(), Uuid::new_v4().to_string().parse().expect("update operation"), selected.id(), role::Update::new(selected.change_id(), serde_json::from_value(json!({"name":"Later definition","level":25,"permissions":[]})).expect("definition")))).await.expect("later role definition");
                    let next = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("later target confirmation");
                    Box::pin(services.appointment().assign(manager.clone(), Uuid::new_v4().to_string().parse().expect("next operation"), command(&next, candidate.role()))).await.expect("unauthorized reader retains a healthy current default definition");
                    let owner = database.connections("postgres").recover().await.expect("isolated maintenance");
                    if matches!(case, "name" | "id_number" | "envelope") {
                        // Damage only the historical reference, keeping the unauthorized reader's current identity healthy.
                        let history = services.appointment().roles.keys();
                        let original_identity = revision::fetch(owner.client(), history, candidate.identity().revision_id()).await.expect("original target identity");
                        Box::pin(services.correction().correct(manager.clone(), Uuid::new_v4().to_string().parse().expect("rename operation"), serde_json::from_value(json!({
                            "target": {"personId": original_identity.person_id(), "revisionId": original_identity.revision_id()},
                            "name": "Healthy Current Identity", "idNumber": original_identity.id_number(),
                        })).expect("current identity input"))).await.expect("complete healthy current identity");
                    }
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("post-startup privileged damage");
                    match case {
                        "operator" => { owner.client().execute("UPDATE access.appointments SET operator_id=gen_random_uuid() WHERE id=$1", &[&original.id()]).await.expect("missing historical operator only"); }
                        "subject" => { owner.client().execute("UPDATE access.appointments SET revision_id=gen_random_uuid() WHERE id=$1", &[&original.id()]).await.expect("missing fixed subject"); }
                        "predecessor" => { owner.client().execute("DELETE FROM access.appointments WHERE id=$1", &[&candidate.assignment_id()]).await.expect("missing required first appointment"); }
                        "proof" => { owner.client().execute("UPDATE access.appointments SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE id=$1", &[&original.id()]).await.expect("damaged retained appointment seal"); }
                        "name" => { owner.client().execute("UPDATE identity.revisions SET name=id_number WHERE id=$1", &[&candidate.identity().revision_id()]).await.expect("valid envelope with wrong name AAD"); }
                        "id_number" => { owner.client().execute("UPDATE identity.revisions SET id_number=set_byte(id_number,octet_length(id_number)-1,get_byte(id_number,octet_length(id_number)-1) # 1) WHERE id=$1", &[&candidate.identity().revision_id()]).await.expect("valid name cannot mask damaged identity number"); }
                        "envelope" => { owner.client().execute("UPDATE identity.revisions SET name=set_byte(name,0,0) WHERE id=$1", &[&candidate.identity().revision_id()]).await.expect("malformed stored envelope"); }
                        "definition" => { owner.client().execute("UPDATE access.changes SET name=$2 WHERE id=$1", &[&selected.change_id(), &changed.name()]).await.expect("damaged fixed definition used by the original appointment"); }
                        "first time" => { owner.client().execute("UPDATE access.appointments SET assigned_at=(SELECT changed_at FROM access.changes WHERE role_id=$2 AND action='create') WHERE id=$1", &[&candidate.assignment_id(), &candidate.role().id()]).await.expect("changed authenticated first appointment time"); }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage persisted");
                    let person = candidate.identity().person_id();
                    let count: i64 = owner.client().query_one("SELECT count(*) FROM access.appointments WHERE person_id=$1", &[&person]).await.expect("independent permanent facts before history read").get(0);
                    assert_eq!(count, if case == "predecessor" { 2 } else { 3 });
                    let filter = || crate::access::appointment::Filter::new(Pagination::new(1,100).expect("page"), Some(person), if case == "first time" {None} else {Some(operation)});
                    assert!(matches!(services.appointment().list(&target, filter()).await, Err(Error::Role(role::Error::Denied))), "authorization precedes damaged history: {case}");
                    if case == "operator" {
                        services.revocation().revoke(target.clone(), Uuid::new_v4().to_string().parse().expect("logout operation"), crate::identity::revocation::Scope::Current).await.expect("invalidate the already constructed reader");
                        let error=services.appointment().list(&target, filter()).await.err().expect("invalid actor precedes damaged history");
                        assert!(matches!(error,Error::Role(role::Error::Current(current::Error::Invalid))));
                        assert_eq!(error.failure(),crate::access::appointment::Failure::Invalid);
                    }
                    assert!(services.appointment().registry.ready().is_ok());
                    let error = services.appointment().list(&manager, filter()).await.err().expect("one corrupt fact rejects the entire page");
                    assert_eq!(error.failure(), crate::access::appointment::Failure::Unavailable, "{case}: {error:?}");
                    assert!(crate::runtime::work::Failure::unreliable(&error), "{case}: {error:?}");
                    match case {
                        "name" | "id_number" => assert!(matches!(error, Error::Revision(crate::identity::person::revision::ReadError::Revision(_)))),
                        "envelope" => assert!(matches!(error, Error::Revision(crate::identity::person::revision::ReadError::Decode(_)))),
                        "predecessor" => assert!(matches!(error, Error::Facts { .. })),
                        _ => assert!(matches!(error, Error::Role(role::Error::History(crate::history::Error::Seal(crate::keys::seal::Error::Mismatch))))),
                    }
                    assert!(services.appointment().registry.ready().is_err());
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.appointments WHERE person_id=$1", &[&person]).await.expect("history never modifies permanent facts").get::<_,i64>(0), count);
                    owner.terminate().await.expect("maintenance ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no calls through damaged reads and drain"); supplier.stop().await;
                })).await.expect("bounded damage case");
            }
        }));
    }

    #[test]
    fn history_batches_full_pages_without_per_appointment_round_trips() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let proxy = Proxy::start(&server).await;
            let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
            let manager = actor(&database, &services, &mut supplier).await;
            for index in 10..109 {
                timeout(Duration::from_secs(60), Box::pin(register_numbered(&database, &services, &mut supplier, index))).await.expect("bounded account preparation");
            }
            timeout(Duration::from_secs(60), Box::pin(async {
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let rows = observer.client().query("SELECT id, person_id, revision_id, operator_id, role_id, operation_id FROM access.appointments ORDER BY assigned_at DESC, id DESC LIMIT 100", &[]).await.expect("committed full page before reading");
                assert_eq!(rows.len(), 100);
                assert_eq!(observer.client().query_one("SELECT count(DISTINCT person_id) FROM access.appointments", &[]).await.expect("distinct actual people").get::<_, i64>(0), 100);
                let mut baseline = None;
                for size in [1, 100] {
                    proxy.arm(Fault::DelayedStateQueries);
                    let started = std::time::Instant::now();
                    let result = services.appointment().list(&manager, crate::access::appointment::Filter::new(Pagination::new(1, size).expect("page"), None, None)).await;
                    let queries = proxy.delayed_queries();
                    eprintln!("Appointment history size={size}: {:?}; queries delayed by 8 ms: {queries}", started.elapsed());
                    let page = result.expect("complete history within the existing read deadline");
                    assert!(queries > 0, "the delayed history transaction was observed");
                    assert_eq!(*baseline.get_or_insert(queries), queries, "a full page uses the same queries as a single row");
                    assert_eq!(page.count(), 100);
                    assert_eq!(page.appointments().len(), usize::try_from(size).expect("page size"));
                    for (appointment, row) in page.appointments().iter().zip(&rows) {
                        assert_eq!(appointment.id(), row.get("id"));
                        assert_eq!(appointment.operation_id(), row.get("operation_id"));
                        assert_eq!(appointment.role().id(), row.get("role_id"));
                        assert_eq!(appointment.subject().person_id(), row.get("person_id"));
                        assert_eq!(appointment.subject().revision_id(), row.get("revision_id"));
                        assert_eq!(appointment.operator().revision_id(), row.get("operator_id"));
                        assert!(appointment.subject().name().ends_with('*'), "only fixed masked identities are disclosed");
                    }
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.appointments", &[]).await.expect("reading preserves facts").get::<_, i64>(0), 100);
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded full history acceptance");
        }));
    }

    #[test]
    fn assignable_reads_keep_one_authorized_snapshot_and_one_deadline() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(20),Box::pin(async {
            let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
            let (context,services)=crate::access::role::tests::start_snapshot(crate::identity::authentication::proxied(&database,&proxy),&supplier).await;
            let manager=actor(&database,&services,&mut supplier).await;let target=register(&database,&services,&mut supplier).await;
            let selected=Box::pin(services.role().create(manager.clone(),Uuid::new_v4().to_string().parse().expect("role operation"),serde_json::from_value(json!({"name":"Read authority","level":300,"permissions":["institution.assignments"]})).expect("definition"))).await.expect("role");
            let candidate=services.appointment().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");
            Box::pin(services.appointment().assign(manager.clone(),Uuid::new_v4().to_string().parse().expect("assignment"),command(&candidate,&selected))).await.expect("real delegated reader");
            proxy.arm(Fault::StalledRoleStates);
            let service=services.appointment().clone();let current=target.clone();
            let task=tokio::spawn(async move{service.roles(&current,Pagination::new(1,100).expect("page")).await});
            timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("actual authenticated state collection after authorization");
            let added=Box::pin(services.role().create(manager.clone(),Uuid::new_v4().to_string().parse().expect("role operation"),serde_json::from_value(json!({"name":"Later eligible","level":20,"permissions":[]})).expect("definition"))).await.expect("later committed definition");
            let next=services.appointment().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("later target confirmation");
            Box::pin(services.appointment().assign(manager.clone(),Uuid::new_v4().to_string().parse().expect("revoke delegation"),command(&next,candidate.role()))).await.expect("lost read permission in a separate transaction");
            let observer=database.connections(&database.user).recover().await.expect("independent state observer");
            proxy.release();let error=timeout(Duration::from_secs(2),task).await.expect("changed snapshot completes its retry").expect("reader").err().expect("new role anchor forces complete authorization again");
            assert!(matches!(error,Error::Role(role::Error::Denied)),"{error:?}");
            assert_eq!(observer.client().query_one("SELECT role_id FROM access.holders WHERE id=$1",&[&candidate.identity().id()]).await.expect("permission loss is already committed").get::<_,role::Id>(0),candidate.role().id());
            assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.roles WHERE id=$1 AND active)",&[&added.id()]).await.expect("new role responsible for the anchor change is committed").get::<_,bool>(0));
            assert!(matches!(services.appointment().roles(&target,Pagination::new(1,20).expect("page")).await,Err(Error::Role(role::Error::Denied))),"next read sees lost permission");
            proxy.arm(Fault::RejectedRollback);
            let error=services.appointment().roles(&target,Pagination::new(1,20).expect("page")).await.err().expect("rejection rollback failure");
            let cause=match &error {Error::Combined{cause,..}=>cause.as_ref(),_=>&error};
            assert!(matches!(cause,Error::Rollback{cause,rollback:crate::database::transaction::EndError::Rollback(_)} if matches!(cause.as_ref(),Error::Role(role::Error::Denied))),"actual rollback preserves refusal and transaction end cause: {error:?}");
            let next=services.appointment().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("new confirmation");
            Box::pin(services.appointment().assign(manager.clone(),Uuid::new_v4().to_string().parse().expect("restore delegation"),command(&next,&selected))).await.expect("restore permission");
            let page=services.appointment().roles(&target,Pagination::new(1,20).expect("page")).await.expect("later authorized snapshot");assert_eq!(page.count(),2);assert!(page.roles().iter().any(|role|role.id()==added.id()));
            // Deadline and finalization checks retain the production read budget.
            services.close(); services.drain().await.expect("snapshot services drained"); context.stop().await.expect("snapshot context stopped");
            let (context, services) = start(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
            let started=tokio::time::Instant::now();proxy.arm(Fault::StalledRoleStates);
            let service=services.appointment().clone();let current=target.clone();let task=tokio::spawn(async move{service.roles(&current,Pagination::new(1,20).expect("page")).await});
            let pid=timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("first wait");
            // State collection and COMMIT share the same deadline; pagination is in memory.
            tokio::time::sleep(Duration::from_millis(1500)).await;proxy.arm(Fault::StalledReadCommit);proxy.release();
            timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("final COMMIT after the one state collection");
            let error=timeout(Duration::from_millis(2300),task).await.expect("deadline did not restart before COMMIT").expect("reader").err().expect("incomplete page is rejected");
            assert!(matches!(error,Error::Timeout(_)),"{error:?}");assert!(started.elapsed()<Duration::from_millis(3900));
            proxy.release();timeout(Duration::from_secs(3),async {loop {if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",&[&pid]).await.expect("actual backend ended").get::<_,bool>(0){break;}tokio::task::yield_now().await;}}).await.expect("timed-out connection was discarded");
            services.appointment().roles(&target,Pagination::new(1,20).expect("page")).await.expect("clean new reader after discard");observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;proxy.stop().await;
        })).await.expect("read boundary deadline");
    }));
    }

    #[test]
    fn confirmation_versions_and_actor_sessions_remain_distinct_from_target_sessions() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(20),Box::pin(async {
            let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;
            let (context,services)=start(database.connections(&database.user),&supplier).await;
            let manager=actor(&database,&services,&mut supplier).await;let target=register(&database,&services,&mut supplier).await;
            let definition=|level|serde_json::from_value(json!({"name":"Confirmed role","level":level,"permissions":["institution.assignments"]})).expect("definition");
            let first=Box::pin(services.role().create(manager.clone(),Uuid::new_v4().to_string().parse().expect("role operation"),definition(20))).await.expect("role");
            let candidate=services.appointment().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("original target confirmation");
            let changed=Box::pin(services.role().update(manager.clone(),Uuid::new_v4().to_string().parse().expect("role operation"),first.id(),role::Update::new(first.change_id(),definition(21)))).await.expect("definition changed");
            let returned=Box::pin(services.role().update(manager.clone(),Uuid::new_v4().to_string().parse().expect("role operation"),first.id(),role::Update::new(changed.change_id(),definition(20)))).await.expect("same values with a new confirmed version");
            assert!(matches!(Box::pin(services.appointment().assign(manager.clone(),Uuid::new_v4().to_string().parse().expect("appointment"),command(&candidate,&first))).await,Err(CommandError::Rejected(Rejection::RoleChanged))));
            let gate=database.connections(&database.user).recover().await.expect("independent role gate");
            gate.client().batch_execute("BEGIN").await.expect("gate transaction");
            let blocker:i32=gate.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("gate backend").get(0);
            gate.client().query_one("SELECT id FROM access.roles WHERE id=$1 FOR UPDATE",&[&returned.id()]).await.expect("hold selected definition");
            let observer=database.connections(&database.user).recover().await.expect("independent lock observer");
            let service=services.role().clone();let current=manager.clone();let id=returned.id();let update=role::Update::new(returned.change_id(),definition(22));
            let changing=tokio::spawn(async move{service.update(current,Uuid::new_v4().to_string().parse().expect("concurrent modification"),id,update).await});
            let modifying=timeout(Duration::from_secs(3),async {loop {let rows=observer.client().query("SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename=current_user AND $1=ANY(pg_catalog.pg_blocking_pids(pid))",&[&blocker]).await.expect("modification waits on actual role gate");if let Some(row)=rows.first(){break row.get::<_,i32>(0);}tokio::task::yield_now().await;}}).await.expect("modification holds institution scope before target row");
            let service=services.appointment().clone();let current=manager.clone();let request=command(&candidate,&returned);
            let assigning=tokio::spawn(async move{service.assign(current,Uuid::new_v4().to_string().parse().expect("concurrent appointment"),request).await});
            timeout(Duration::from_secs(3),async {loop {if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks lock JOIN pg_catalog.pg_stat_activity activity ON activity.pid=lock.pid WHERE activity.datname=current_database() AND activity.usename=current_user AND lock.locktype='advisory' AND NOT lock.granted AND $1=ANY(pg_catalog.pg_blocking_pids(lock.pid)))",&[&modifying]).await.expect("assignment waits behind actual modification scope").get::<_,bool>(0){break;}tokio::task::yield_now().await;}}).await.expect("scope wait observed");
            gate.client().batch_execute("COMMIT").await.expect("release role gate");gate.terminate().await.expect("gate ended");
            let returned=timeout(Duration::from_secs(3),changing).await.expect("bounded modification").expect("modification work").expect("new definition committed");
            assert!(matches!(timeout(Duration::from_secs(3),assigning).await.expect("bounded appointment").expect("appointment work"),Err(CommandError::Rejected(Rejection::RoleChanged))),"scope wait must recheck the selected change");
            assert_eq!(observer.client().query_one("SELECT count(*) FROM access.appointments WHERE person_id=$1",&[&candidate.identity().person_id()]).await.expect("neither stale confirmation produced history").get::<_,i64>(0),1);
            services.revocation().revoke(target,Uuid::new_v4().to_string().parse().expect("target logout"),crate::identity::revocation::Scope::All).await.expect("target logs out before assignment");
            let operation=Uuid::new_v4().to_string().parse().expect("original operation");
            let original=Box::pin(services.appointment().assign(manager.clone(),operation,command(&candidate,&returned))).await.expect("target sessions are not assignment authority");
            assert_eq!(observer.client().query_one("SELECT count(*) FROM access.appointments WHERE person_id=$1",&[&candidate.identity().person_id()]).await.expect("only the confirmed update persisted").get::<_,i64>(0),2);
            assert!(observer.client().query_one("SELECT appointment.assigned_at > change.changed_at AND appointment.role_id=change.role_id FROM access.appointments appointment JOIN access.changes change ON change.id=$2 WHERE appointment.id=$1",&[&original.id(),&returned.change_id()]).await.expect("independent committed assignment follows its confirmed definition before replay").get::<_,bool>(0));
            services.revocation().revoke(manager.clone(),Uuid::new_v4().to_string().parse().expect("actor logout"),crate::identity::revocation::Scope::Current).await.expect("original actor session revoked");
            let error=Box::pin(services.appointment().assign(manager,operation,command(&candidate,&returned))).await.err().expect("an original result does not bypass current actor qualification");
            assert!(matches!(error,CommandError::Unauthenticated));
            assert_eq!(error.code(),Some("authentication_invalid"));
            assert_eq!(observer.client().query_one("SELECT count(*) FROM access.appointments WHERE person_id=$1",&[&candidate.identity().person_id()]).await.expect("invalid original waiter creates no appointment").get::<_,i64>(0),2);
            let manager=actor(&database,&services,&mut supplier).await;
            let message_id=message(&database,services.authentication(),&mut supplier,"13800000000").await;
            let Receipt::Session(issued)=Box::pin(services.authentication().create_session(input(Uuid::new_v4(),message_id,"13800000000",CODE))).await.expect("target logs in again") else {panic!("existing account session");};
            let other=services.current().authenticate(current::Proof{id:issued.id(),token:issued.token().text().parse().expect("proof")}).await.expect("new session with assignment permission");
            assert!(matches!(Box::pin(services.appointment().assign(other,operation,command(&candidate,&returned))).await,Err(CommandError::Mismatch)),"different permitted person cannot claim the original operation");
            assert_eq!(Box::pin(services.appointment().assign(manager.clone(),operation,command(&candidate,&returned))).await.expect("same person through a new session").id(),original.id());
            observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");drop(services);
            let (context,services)=start(database.connections(&database.user),&supplier).await;
            let replay=Box::pin(services.appointment().assign(manager,operation,command(&candidate,&returned))).await.expect("startup retains original proof and definitions");assert_eq!(replay.id(),original.id());assert_eq!(replay.assigned_at(),original.assigned_at());
            services.close();services.drain().await.expect("drained after restart");context.stop().await.expect("stopped after restart");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err(),"monitor after actual logins through original replay and restart drain");supplier.stop().await;
        })).await.expect("identity and confirmation lifetime deadline");
    }));
    }

    #[test]
    fn real_replacement_preserves_original_input_and_definition_after_later_changes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(25), Box::pin(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(database.connections(&database.user), &supplier).await;
            let manager = actor(&database, &services, &mut supplier).await;
            let id = message(&database, services.authentication(), &mut supplier, "13800000000").await;
            let Receipt::Registration(credential) = Box::pin(services.authentication().create_session(input(Uuid::new_v4(), id, "13800000000", CODE))).await.expect("R1") else { panic!("registration credential"); };
            let operation: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
            let registered = services.registration().register(crate::registration::Input::new(operation, credential.id(), credential.token().text().parse().expect("proof"), "Target Person".parse().expect("name"), "00000020000229007X".parse().expect("number"))).await.expect("R2 account");
            let target = services.current().authenticate(current::Proof { id: registered.id(), token: registered.token().text().parse().expect("session proof") }).await.expect("target actor");
            let original = services.role().read_current(&target).await.expect("ordinary current role");
            let initial=services.appointment().list(&manager, crate::access::appointment::Filter::new(Pagination::new(1,100).expect("page"),None,None)).await.expect("initialization same-time exception and ordinary first appointment");
            assert_eq!(initial.count(),2);assert!(initial.appointments().iter().all(|appointment|appointment.sequence()==1));assert!(initial.appointments().iter().any(|appointment|i16::from(appointment.role().level())==1000));
            let selected = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("create operation"), serde_json::from_value(json!({"name":"Assignable","level":200,"permissions":["institution.accounts"]})).expect("role input"))).await.expect("complete custom role");
            let candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("exact eligible target");
            assert_eq!(candidate.identity().name(), "T*"); assert_eq!(candidate.identity().suffix(), "007X"); assert_eq!(candidate.role().id(), original.id());
            assert!(matches!(services.appointment().candidate(&manager, "13900000000".parse().expect("self mobile")).await, Err(Error::Absent)));
            let list = services.appointment().roles(&manager, Pagination::new(1,100).expect("page")).await.expect("assignable definitions");
            assert_eq!(list.roles().iter().map(|role| role.id()).collect::<Vec<_>>(), [selected.id(), original.id()]);
            let first = Box::pin(services.appointment().assign(manager.clone(), operation, command(&candidate, &selected))).await.expect("online operation may reuse R2 operation UUID");
            assert_eq!(first.sequence(), 2); assert_eq!(first.role().id(), selected.id());
            let observer = database.connections(&database.user).recover().await.expect("independent runtime SQL");
            let row = observer.client().query_one("SELECT person.sequence AS person_sequence, holder.role_id, holder.assignment_id, (SELECT count(*) FROM access.appointments WHERE person_id=person.id) AS count, (SELECT octet_length(seal) FROM access.appointments WHERE id=holder.assignment_id) AS size FROM identity.persons person JOIN access.holders holder ON holder.person_id=person.id WHERE person.id=$1", &[&candidate.identity().person_id()]).await.expect("complete persistent graph before replay");
            assert_eq!(row.get::<_,i64>("person_sequence"), 1); assert_eq!(row.get::<_,role::Id>("role_id"), selected.id()); assert_eq!(row.get::<_,crate::access::appointment::Id>("assignment_id"), first.id()); assert_eq!(row.get::<_,i64>("count"), 2); assert_eq!(row.get::<_,i32>("size"), 32);
            assert_eq!(services.role().read_current(&target).await.expect("immediate live authority").id(), selected.id());
            let changed=Box::pin(services.role().update(manager.clone(),Uuid::new_v4().to_string().parse().expect("update operation"),selected.id(),role::Update::new(selected.change_id(),serde_json::from_value(json!({"name":"Renamed later","level":201,"permissions":[]})).expect("updated definition")))).await.expect("later definition does not rewrite assignment");
            let second_candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("new confirmation");
            let second = Box::pin(services.appointment().assign(manager.clone(), Uuid::new_v4().to_string().parse().expect("second operation"), command(&second_candidate, &original))).await.expect("replace with default"); assert_eq!(second.sequence(), 3);
            assert!(matches!(Box::pin(services.appointment().assign(manager.clone(), Uuid::new_v4().to_string().parse().expect("new operation with old confirmation"), command(&candidate,&selected))).await,Err(CommandError::Rejected(Rejection::TargetChanged))), "returning to the same role does not revive the old appointment confirmation");
            Box::pin(services.role().delete(manager.clone(), Uuid::new_v4().to_string().parse().expect("delete operation"), selected.id(), role::Delete::new(changed.change_id()))).await.expect("old role may disappear");
            assert_eq!(observer.client().query_one("SELECT count(*) FROM access.appointments WHERE person_id=$1", &[&candidate.identity().person_id()]).await.expect("all permanent facts before replay").get::<_,i64>(0),3);
            let confirmation=services.correction().candidate_by_person(&manager,first.subject().person_id()).await.expect("later real-name confirmation");
            let corrected=Box::pin(services.correction().correct(manager.clone(),Uuid::new_v4().to_string().parse().expect("correction operation"),serde_json::from_value(json!({"target":confirmation.target(),"name":"Later Target","idNumber":"00000020000229007X"})).expect("correction input"))).await.expect("real later target identity");
            let subject_revision=corrected.correction().id().revision_id();
            let cancelled=Box::pin(services.cancellation().cancel_account(
                manager.clone(), Uuid::new_v4().to_string().parse().expect("cancellation operation"),
                candidate.identity().id(), serde_json::from_value(json!({
                    "personId":candidate.identity().person_id(), "revisionId":subject_revision,
                    "mobile":"13800000000",
                })).expect("current cancellation confirmation"),
            )).await.expect("real later target cancellation");
            services.close(); services.drain().await.expect("online work drained");
            context.stop().await.expect("online instance released for maintenance");

            let keys = crate::identity::authentication::inventory(crate::identity::authentication::keys());
            let input = serde_json::from_value(json!({
                "operator": {"mobile":"13900000000"},
                "target": {"personId": first.operator().person_id(), "revisionId": first.operator().revision_id()},
                "name":"Later Operator", "idNumber":"000000200002290010",
            })).expect("system correction input");
            let maintenance = crate::runtime::Context::new(database.connections(&database.user)).await.expect("maintenance instance");
            let barrier = crate::database::Barrier::begin(maintenance.clone()).await.expect("maintenance startup barrier");
            let administration = crate::startup::Administration::start(
                barrier, keys,
                crate::identity::authentication::session_settings().app_id().clone(),
            ).await.expect("verified maintenance application");
            let completion = Box::pin(administration.correct(
                Uuid::new_v4().to_string().parse().expect("correction operation"), input, false,
            )).await.expect("real system correction").expect("committed correction");
            let operator_revision = completion.id().revision_id();
            administration.close(); administration.drain().await.expect("maintenance work drained");
            maintenance.stop().await.expect("maintenance instance released");
            let (context, services) = start(database.connections(&database.user), &supplier).await;
            assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.accounts WHERE id=$1 AND person_id=$2 AND cancellation_id=$3 AND mobile IS NULL AND id_number IS NULL AND suspension_id IS NULL)",&[&candidate.identity().id(),&candidate.identity().person_id(),&cancelled.id()]).await.expect("independent target tombstone before replay").get::<_,bool>(0));
            assert_eq!(observer.client().query_one("SELECT revision_id FROM identity.persons WHERE id=$1", &[&first.subject().person_id()]).await.expect("new subject pointer before history read").get::<_,crate::identity::person::revision::Id>(0),subject_revision);
            assert_eq!(observer.client().query_one("SELECT revision_id FROM identity.persons WHERE id=$1", &[&first.operator().person_id()]).await.expect("new operator pointer before history read").get::<_,crate::identity::person::revision::Id>(0),operator_revision);
            let filter=|page,size,person,operation|crate::access::appointment::Filter::new(Pagination::new(page,size).expect("page"),person,operation);
            let history=services.appointment().list(&manager,filter(1,100,Some(candidate.identity().person_id()),None)).await.expect("history independent of current account and role");
            assert_eq!(history.count(),3);assert_eq!(history.appointments().iter().map(|appointment|appointment.sequence()).collect::<Vec<_>>(),[3,2,1]);
            let historical=&history.appointments()[1];assert_eq!(historical.id(),first.id());assert_eq!(historical.role().change_id(),selected.change_id());assert_eq!(historical.role().name(),selected.name());assert_eq!(historical.role().permissions(),selected.permissions());assert_eq!(historical.subject().revision_id(),candidate.identity().revision_id());assert_eq!(historical.subject().name(),"T*");assert_eq!(historical.operator().revision_id(),first.operator().revision_id());
            assert_eq!(historical.operator().name(),first.operator().name());
            let matches=services.appointment().list(&manager,filter(1,100,None,Some(operation))).await.expect("two operation families");assert_eq!(matches.count(),2);assert_eq!(matches.appointments().iter().map(|appointment|appointment.sequence()).collect::<Vec<_>>(),[2,1]);
            let page=services.appointment().list(&manager,filter(2,1,Some(candidate.identity().person_id()),Some(operation))).await.expect("combined filtered pagination");assert_eq!(page.count(),2);assert_eq!(page.appointments()[0].sequence(),1);
            let empty=services.appointment().list(&manager,filter(20,100,None,None)).await.expect("past last page");assert_eq!(empty.count(),4);assert!(empty.appointments().is_empty());
            let empty=services.appointment().list(&manager,filter(1,100,None,Some(Uuid::new_v4().to_string().parse().expect("unmatched operation")))).await.expect("no original match");assert_eq!(empty.count(),0);assert!(empty.appointments().is_empty());
            let replay = Box::pin(services.appointment().assign(manager.clone(), operation, command(&candidate, &selected))).await.expect("original completed appointment");
            assert_eq!(replay.id(), first.id()); assert_eq!(replay.role().change_id(), selected.change_id()); assert_eq!(replay.assigned_at(), first.assigned_at());
            assert!(matches!(Box::pin(services.appointment().assign(manager, operation, command(&second_candidate, &original))).await, Err(CommandError::Mismatch)));
            observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("accepted work drained"); context.stop().await.expect("clock ended");
            assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no extra supplier calls through replacement/replay/drain"); supplier.stop().await;
        })).await.expect("lifecycle deadline");
    }));
    }
}
