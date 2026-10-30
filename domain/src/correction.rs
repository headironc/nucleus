//! Website real-name correction coordinates Access qualification and immutable Identity facts.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use platform::database::Lease;
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Client;
use utoipa::ToSchema;

use crate::{
    access::{self, appointment, role},
    database::{
        Query,
        transaction::{self, EndError},
    },
    identity::{
        account,
        correction::{self, Correction},
        current::{self, Actor},
        keys::Keys,
        mobile::Mobile,
        operation,
        person::{self, name::Name, number::IdNumber, revision},
    },
    kernel::{self, App, Authorization, Command, completion, tx::Tx},
    pagination::Pagination,
    runtime::{self, Context, registry::Registry, work},
};

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = correction::Target)]
pub struct Target {
    person_id: person::Id,
    revision_id: revision::Id,
}

impl Target {
    pub fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub fn revision_id(&self) -> revision::Id {
        self.revision_id
    }
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = correction::Input)]
pub struct Input {
    target: Target,
    name: Name,
    id_number: IdNumber,
}

pub struct Candidate {
    target: Target,
    name: Name,
    id_number: IdNumber,
}

impl Candidate {
    pub fn target(&self) -> &Target {
        &self.target
    }
    pub fn name(&self) -> &Name {
        &self.name
    }
    pub fn id_number(&self) -> &IdNumber {
        &self.id_number
    }
}

pub struct Receipt {
    correction: Correction,
    created: bool,
}

impl Receipt {
    pub fn correction(&self) -> &Correction {
        &self.correction
    }
    pub fn created(&self) -> bool {
        self.created
    }
}

struct Read {
    lease: Lease,
    deadline: Instant,
}

#[derive(Clone)]
pub struct Service {
    app: App,
    context: Context,
    current: current::Service,
    roles: role::Reader,
    keys: Arc<Keys>,
    accounts: account::Reader,
    persons: person::Reader,
    registry: Arc<Registry>,
}

impl Service {
    pub(crate) fn prepare(app: App, current: current::Service, keys: Arc<Keys>) -> Self {
        Self {
            context: app.context().clone(),
            current,
            registry: Arc::clone(app.registry()),
            roles: role::Reader::new(app.clone()),
            accounts: account::Reader::new(app.clone(), Arc::clone(&keys)),
            persons: person::Reader::new(
                account::Reader::new(app.clone(), Arc::clone(&keys)),
                Arc::clone(&keys),
            ),
            app,
            keys,
        }
    }

    async fn authorize(
        &self,
        client: &Client,
        roles: &role::Reader,
        actor: &Actor,
    ) -> Result<(role::Role, person::Id, revision::Id, DateTime<Utc>), Error> {
        Ok(access::authorize(
            client,
            roles,
            &self.current,
            actor,
            role::Code::InstitutionCorrections,
        )
        .await?)
    }

    async fn authorize_target(
        &self,
        client: &Client,
        roles: &role::Reader,
        person_id: person::Id,
        operator_id: person::Id,
        role: &role::Role,
        now: DateTime<Utc>,
    ) -> Result<Option<appointment::Id>, Error> {
        if person_id == operator_id {
            return Err(Error::Absent);
        }
        if let Some(account_id) = self
            .accounts
            .with_read(roles.read()?)
            .find_by_person(client, person_id, now)
            .await?
        {
            let target = appointment::find_by_account(client, roles, account_id, now)
                .await?
                .ok_or(correction::Error::Facts {
                    rule: "current target account retains its institutional assignment",
                })?;
            if target.role().level() >= role.level() {
                return Err(Error::Absent);
            }
            return Ok(Some(target.id()));
        } else if !correction::person_exists(client, person_id).await? {
            return Err(Error::Absent);
        }
        Ok(None)
    }

    async fn target(
        &self,
        client: &Client,
        roles: &role::Reader,
        person_id: person::Id,
        operator_id: person::Id,
        role: &role::Role,
        now: DateTime<Utc>,
    ) -> Result<(revision::Profile, Option<appointment::Id>), Error> {
        let appointment_id = self
            .authorize_target(client, roles, person_id, operator_id, role, now)
            .await?;
        let profile = self
            .persons
            .with_read(roles.read()?)
            .profile(client, person_id, now)
            .await?
            .ok_or(correction::Error::Facts {
                rule: "authorized correction target retains its permanent person",
            })?;
        Ok((profile, appointment_id))
    }

    pub async fn candidate_by_mobile(
        &self,
        actor: &Actor,
        mobile: Mobile,
    ) -> Result<Candidate, Error> {
        let read = self.lease().await?;
        let client = read.lease.client();
        let budget = self.context.read_budget();
        let result = timeout_at(
            read.deadline,
            budget.run(client, self.roles.anchors(), |view| {
                let roles = self.roles.at(view);
                let mobile = &mobile;
                async move {
                    let (role, operator_id, _, now) = self.authorize(client, &roles, actor).await?;
                    let account_id = self
                        .accounts
                        .at(view)
                        .find_by_mobile(client, mobile, now)
                        .await?
                        .ok_or(Error::Absent)?;
                    let target = appointment::find_by_account(client, &roles, account_id, now)
                        .await?
                        .ok_or(Error::Absent)?;
                    if target.person_id() == operator_id || target.role().level() >= role.level() {
                        return Err(Error::Absent);
                    }
                    // Opening the account envelope verifies that the lookup digest names this exact mobile.
                    self.accounts
                        .at(view)
                        .find_by_id(client, account_id, mobile, now)
                        .await?
                        .ok_or(Error::Absent)?;
                    let profile = self
                        .persons
                        .with_read(roles.read()?)
                        .profile(client, target.person_id(), now)
                        .await?
                        .ok_or(correction::Error::Facts {
                            rule: "located account retains its person",
                        })?;
                    let (revision_id, name, id_number) = profile.into_values();
                    Ok(Candidate {
                        target: Target {
                            person_id: target.person_id(),
                            revision_id,
                        },
                        name,
                        id_number,
                    })
                }
            }),
        )
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(read, result).await
    }

    pub async fn candidate_by_person(
        &self,
        actor: &Actor,
        person_id: person::Id,
    ) -> Result<Candidate, Error> {
        let read = self.lease().await?;
        let client = read.lease.client();
        let budget = self.context.read_budget();
        let result = timeout_at(
            read.deadline,
            budget.run(client, self.roles.anchors(), |view| async move {
                let roles = self.roles.at(view);
                let (role, operator_id, _, now) = self.authorize(client, &roles, actor).await?;
                let (profile, _) = self
                    .target(client, &roles, person_id, operator_id, &role, now)
                    .await?;
                let (revision_id, name, id_number) = profile.into_values();
                Ok(Candidate {
                    target: Target {
                        person_id,
                        revision_id,
                    },
                    name,
                    id_number,
                })
            }),
        )
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(read, result).await
    }

    pub async fn history(
        &self,
        actor: &Actor,
        pagination: Pagination,
    ) -> Result<correction::history::List, Error> {
        let read = self.lease().await?;
        let result = timeout_at(read.deadline, async {
            let client = read.lease.client();
            self.context
                .read_budget()
                .run(client, self.roles.anchors(), |view| async move {
                    let (person_id, _, now) =
                        self.current.at(view).fetch_person(client, actor).await?;
                    Ok(correction::history::fetch_list(
                        client,
                        self.roles.keys(),
                        self.roles.anchors(),
                        crate::history::Read::Snapshot(view),
                        person_id,
                        pagination,
                        now,
                    )
                    .await?)
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(read, result).await
    }

    pub async fn history_by_person(
        &self,
        actor: &Actor,
        person_id: person::Id,
        pagination: Pagination,
    ) -> Result<correction::history::management::List, Error> {
        let read = self.lease().await?;
        let client = read.lease.client();
        let budget = self.context.read_budget();
        let result = timeout_at(
            read.deadline,
            budget.run(client, self.roles.anchors(), |view| async move {
                let roles = self.roles.at(view);
                let (role, operator_id, _, now) = self.authorize(client, &roles, actor).await?;
                self.authorize_target(client, &roles, person_id, operator_id, &role, now)
                    .await?;
                Ok(correction::history::management::fetch_list(
                    client,
                    roles.keys(),
                    roles.anchors(),
                    crate::history::Read::Snapshot(view),
                    person_id,
                    pagination,
                    now,
                )
                .await?)
            }),
        )
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(read, result).await
    }

    async fn lease(&self) -> Result<Read, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        Ok(Read { lease, deadline })
    }

    async fn finish<T>(&self, read: Read, result: Result<T, Error>) -> Result<T, Error> {
        let Read { lease, deadline } = read;
        if let Err(error) = &result
            && work::Failure::unreliable(error)
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
            Err(error) if error.rejected() => Err(Self::rollback(lease, error, deadline).await),
            Err(error) => Err(error.combine(lease.discard().await)),
        }
    }

    pub async fn correct(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<Receipt, kernel::Error<Rejection>> {
        self.app
            .command(
                operation_id,
                Write {
                    service: self.clone(),
                    actor,
                    input,
                    id: correction::Id::allocate(),
                },
            )
            .await
    }

    async fn rollback(lease: Lease, cause: Error, deadline: Instant) -> Error {
        let (known, cleanup) =
            transaction::rollback(cause, deadline, lease.client().batch_execute("ROLLBACK"))
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Absent,
    TargetChanged,
    Unchanged,
    IdNumberTaken,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Absent => kernel::Status::NotFound,
            Self::TargetChanged | Self::Unchanged | Self::IdNumberTaken => kernel::Status::Conflict,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Absent => "target_unavailable",
            Self::TargetChanged => "target_changed",
            Self::Unchanged => "state_conflict",
            Self::IdNumberTaken => "id_number_taken",
        }
    }
}

struct Write {
    service: Service,
    actor: Actor,
    input: Input,
    id: correction::Id,
}

struct Facts {
    person: crate::history::Verified<person::state::State>,
    before: revision::Profile,
    appointment_id: Option<appointment::Id>,
    account: Option<crate::history::Verified<account::state::State>>,
    operator_id: revision::Id,
    occupied: bool,
}

impl Command for Write {
    const COMPLETIONS: completion::Template = correction::ONLINE;
    type Facts = Facts;
    type Change = (Facts, DateTime<Utc>);
    type Output = Receipt;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(
        &self,
        authority: &Authorization,
    ) -> Result<completion::Request, kernel::Error<Rejection>> {
        completion::Request::new(
            "correct",
            self.input.target.person_id.to_string(),
            &self.input,
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), kernel::Error<Rejection>> {
        tx.shared(b"access/scope", b"institution").await?;
        let target =
            account::state::discover_person(tx.client(), self.input.target.person_id).await?;
        match target {
            Some(target) => self.actor.lock_accounts(tx.client(), target).await?,
            None => self.actor.lock_account(tx.client()).await?,
        }
        correction::lock_person(tx.client(), self.input.target.person_id).await?;
        kernel::tx::object(
            tx.client(),
            b"identity/id_number",
            self.input.id_number.as_ref().as_bytes(),
        )
        .await?;
        self.actor.lock_session(tx.client()).await?;
        if target
            != account::state::discover_person(tx.client(), self.input.target.person_id).await?
        {
            return Err(kernel::Error::lock_set_changed());
        }
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, kernel::Error<Rejection>> {
        let subject = subject.ok_or(kernel::Error::corrupt(
            "online correction declares its session",
        ))?;
        let (_, person_id, revision_id, now) = access::authorize_subject(
            tx.client(),
            &self.service.roles,
            subject,
            role::Code::InstitutionCorrections,
        )
        .await?;
        Ok(Authorization {
            person_id,
            revision_id,
            now,
        })
    }

    async fn absent(&self, tx: &Tx) -> Result<(), kernel::Error<Rejection>> {
        correction::absent(tx, &self.service.app, correction::Source::Online).await
    }

    async fn load(
        &self,
        tx: &Tx,
        authority: &Authorization,
    ) -> Result<Facts, kernel::Error<Rejection>> {
        if authority.person_id == self.input.target.person_id {
            return Err(kernel::Error::Rejected(Rejection::Absent));
        }
        let now = authority.now;
        let manager = appointment::find_by_account(
            tx.client(),
            &self.service.roles,
            self.actor.account_id(),
            now,
        )
        .await?
        .ok_or(kernel::Error::corrupt(
            "an authorized correction manager retains its role",
        ))?;
        let account = self
            .service
            .accounts
            .person_state(tx.client(), self.input.target.person_id, now)
            .await?;
        let appointment_id = if let Some(account) = account
            .as_ref()
            .filter(|account| account.current().is_some())
        {
            let target =
                appointment::find_by_account(tx.client(), &self.service.roles, account.id(), now)
                    .await?
                    .ok_or(kernel::Error::corrupt(
                        "a current correction target retains its role",
                    ))?;
            if target.role().level() >= manager.role().level() {
                return Err(kernel::Error::Rejected(Rejection::Absent));
            }
            Some(target.id())
        } else {
            None
        };
        let before = self
            .service
            .persons
            .profile(tx.client(), self.input.target.person_id, now)
            .await?
            .ok_or(kernel::Error::Rejected(Rejection::Absent))?;
        if before.revision_id() != self.input.target.revision_id {
            return Err(kernel::Error::Rejected(Rejection::TargetChanged));
        }
        let person = crate::identity::person::state::find(
            tx.client(),
            self.service.app.keys(),
            self.service.app.anchors(),
            crate::history::Read::Locked,
            self.input.target.person_id,
            now,
        )
        .await?
        .ok_or(kernel::Error::Rejected(Rejection::Absent))?;
        if person.revision_id() != before.revision_id() {
            return Err(kernel::Error::corrupt(
                "the current person state retains its correction target revision",
            ));
        }
        if before.name().as_ref() == self.input.name.as_ref()
            && before.id_number().as_ref() == self.input.id_number.as_ref()
        {
            return Err(kernel::Error::Rejected(Rejection::Unchanged));
        }
        let occupied = self
            .service
            .accounts
            .number_occupied(
                tx.client(),
                &self.input.id_number,
                self.input.target.person_id,
                now,
            )
            .await?;
        Ok(Facts {
            person,
            before,
            appointment_id,
            account,
            operator_id: authority.revision_id,
            occupied,
        })
    }

    fn decide(
        &self,
        facts: Facts,
        now: DateTime<Utc>,
    ) -> Result<Self::Change, kernel::Error<Rejection>> {
        if facts.occupied {
            return Err(kernel::Error::Rejected(Rejection::IdNumberTaken));
        }
        Ok((facts, now))
    }

    async fn apply(
        &self,
        tx: &Tx,
        (facts, now): Self::Change,
        pending: &mut Option<crate::history::Pending>,
    ) -> Result<Receipt, kernel::Error<Rejection>> {
        let correction = correction::write(
            tx.client(),
            self.service.app.keys(),
            self.service.app.anchors(),
            &self.service.keys.number,
            &facts.person,
            &facts.before,
            facts.account.as_ref(),
            pending,
            correction::New {
                id: self.id,
                person_id: self.input.target.person_id,
                operator_id: facts.operator_id,
                appointment_id: facts.appointment_id,
                name: &self.input.name,
                id_number: &self.input.id_number,
                operation_id: tx.operation_id(),
                corrected_at: now,
            },
        )
        .await
        .map_err(write_error)?;
        Ok(Receipt {
            correction,
            created: true,
        })
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Receipt, kernel::Error<Rejection>> {
        let original = correction::find_by_operation(
            tx.client(),
            self.service.app.keys(),
            tx.operation_id(),
            now,
        )
        .await?
        .ok_or(kernel::Error::corrupt(
            "an online completion retains its original correction",
        ))?;
        if original.source() != correction::Source::Online
            || !original.matches(
                self.input.target.person_id,
                self.input.target.revision_id,
                request.person_id(),
                &self.input.name,
                &self.input.id_number,
            )
        {
            return Err(kernel::Error::corrupt(
                "online completion and permanent correction agree",
            ));
        }
        let correction = original.into_correction();
        Ok(Receipt {
            created: correction.id() == self.id,
            correction,
        })
    }
}

fn write_error(error: correction::Error) -> kernel::Error<Rejection> {
    match error {
        correction::Error::IdNumberTaken => kernel::Error::Rejected(Rejection::IdNumberTaken),
        error => error.into(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the target is unavailable")]
    Absent,
    #[error("{0}")]
    Current(#[from] current::Error),
    #[error("{0}")]
    Role(#[from] role::Error),
    #[error("{0}")]
    Appointment(#[from] appointment::Error),
    #[error("{0}")]
    Account(#[from] account::ReadError),
    #[error("{0}")]
    Person(#[from] person::ReadError),
    #[error("{0}")]
    Fact(#[from] correction::Error),
    #[error("correction query failed: {0}")]
    Query(#[from] Query),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("{0}")]
    Work(#[from] runtime::work::Error),
    #[error("correction coordination or digest computation failed")]
    Crypto(#[from] platform::cryptography::Error),
    #[error("correction transaction end failed: {0}")]
    End(#[source] EndError),
    #[error("correction budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("correction rejection rollback failed")]
    Rollback {
        #[source]
        cause: Box<Error>,
        rollback: EndError,
    },
    #[error("correction and connection termination failed")]
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

impl From<crate::history::Error> for Error {
    fn from(error: crate::history::Error) -> Self {
        Self::Role(role::Error::History(error))
    }
}

impl crate::database::read::Failure for Error {
    fn changed(&self) -> bool {
        crate::database::read::changed(self)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum Failure {
    Invalid,
    Denied,
    Absent,
    Unavailable,
}

impl Error {
    pub fn failure(&self) -> Failure {
        match self {
            Self::Current(current::Error::Invalid)
            | Self::Role(role::Error::Current(current::Error::Invalid)) => Failure::Invalid,
            Self::Role(role::Error::Denied) => Failure::Denied,
            Self::Absent => Failure::Absent,
            _ => Failure::Unavailable,
        }
    }
    fn rejected(&self) -> bool {
        matches!(
            self.failure(),
            Failure::Invalid | Failure::Denied | Failure::Absent
        )
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

impl work::Failure for Error {
    fn unreliable(&self) -> bool {
        match self {
            Self::Current(error) => error.unreliable(),
            Self::Role(error) => work::Failure::unreliable(error),
            Self::Appointment(error) => work::Failure::unreliable(error),
            Self::Account(error) => error.unreliable(),
            Self::Person(error) => error.unreliable(),
            Self::Fact(error) => error.unreliable(),
            Self::Combined { cause, .. } | Self::Rollback { cause, .. } => {
                work::Failure::unreliable(cause.as_ref())
            }
            _ => false,
        }
    }
    fn report(&self, operation_id: operation::Id) {
        if !self.rejected() {
            tracing::error!(%operation_id, stage = "correction_completion", kind = self.failure().as_ref(), error = ?self);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::expect_used)]
    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::time::timeout;
    use uuid::Uuid;

    use crate::{
        access::{
            appointment,
            role::tests::{actor, start},
        },
        correction::{Candidate, Error, Input},
        identity::{
            authentication::{self, Database, Supplier, keys, server},
            correction, operation,
            person::{self, revision},
        },
        pagination::Pagination,
    };

    fn input(candidate: &Candidate, name: &str, id_number: &str) -> Input {
        serde_json::from_value(
            json!({"target":candidate.target(),"name":name,"idNumber":id_number}),
        )
        .expect("typed correction")
    }

    pub(crate) async fn rename_administrator(
        services: &crate::startup::Services,
        person: person::Id,
        before: revision::Id,
        name: &str,
        number: &str,
    ) -> revision::Id {
        let service = services.correction();
        Box::pin(correction::admin::run(
            service.app.clone(),
            Arc::clone(&service.keys),
            Uuid::new_v4()
                .to_string()
                .parse()
                .expect("rename operation"),
            serde_json::from_value(json!({
                "operator": {"mobile": "13900000000"},
                "target": {"personId": person, "revisionId": before},
                "name": name, "idNumber": number,
            }))
            .expect("administrator rename"),
            false,
        ))
        .await
        .expect("complete administrator correction")
        .expect("committed correction")
        .id()
        .revision_id()
    }

    #[test]
    fn accepted_corrections_recover_after_session_revocation() {
        use fixture::database_proxy::Proxy;

        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let candidate = services.correction().candidate_by_mobile(&manager, "13800000000".parse().expect("mobile")).await.expect("confirmed identity");
                let command = || input(&candidate, "Recovered correction", "00000020000229007X");
                let operation = Uuid::new_v4().to_string().parse().expect("operation");
                let result = Box::pin(crate::recovery::after_revocation(&database, &proxy, services.revocation(), &manager, services.correction().correct(manager.clone(), operation, command()))).await;
                let observer = database.connections(&database.user).recover().await.expect("independent facts");
                let row = observer.client().query_one("SELECT correction.id,correction.sequence,person.sequence AS person_sequence FROM identity.corrections correction JOIN identity.revisions revision ON revision.id=correction.id AND revision.person_id=correction.person_id JOIN identity.persons person ON person.id=correction.person_id AND person.revision_id=revision.id WHERE correction.operation_id=$1 AND correction.person_id=$2", &[&operation,&candidate.target().person_id()]).await.expect("one permanent correction and its current revision");
                assert_eq!((row.get::<_,i64>("sequence"),row.get::<_,i64>("person_sequence")), (2,2));
                if let Ok(original) = &result { assert_eq!(original.correction().id(),row.get("id")); assert!(original.created()); }
                let Err(error) = Box::pin(services.correction().correct(manager, operation, command())).await else { panic!("new request must be refused"); };
                assert!(matches!(error, crate::kernel::Error::Unauthenticated), "{error:?}");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.corrections WHERE operation_id=$1", &[&operation]).await.expect("no replay writes").get::<_,i64>(0), 1);
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.ready().await.expect("correction history survives restart");
                services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped");
                observer.terminate().await.expect("observer ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                assert!(result.is_ok(), "accepted correction lost its result: {:?}",result.as_ref().err());
            })).await.expect("correction recovery after session loss deadline");
        }));
    }

    pub(crate) async fn register_person(
        database: &Database,
        services: &crate::startup::Services,
        supplier: &mut Supplier,
        mobile: &str,
        name: &str,
        id_number: &str,
    ) -> crate::identity::current::Actor {
        use crate::identity::authentication::session::{
            Receipt,
            tests::{CODE, input, message},
        };

        let id = message(database, services.authentication(), supplier, mobile).await;
        let Receipt::Registration(credential) = Box::pin(
            services
                .authentication()
                .create_session(input(Uuid::new_v4(), id, mobile, CODE)),
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
                name.parse().expect("name"),
                id_number.parse().expect("identity number"),
            ))
            .await
            .expect("real R2 account");
        services
            .current()
            .authenticate(crate::identity::current::Proof {
                id: registered.id(),
                token: registered.token().text().parse().expect("session proof"),
            })
            .await
            .expect("registered actor")
    }

    #[test]
    fn self_history_discloses_complete_pages_in_personal_order_without_management_authority() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let empty = services.correction().history(&target, Pagination::new(1, 20).expect("default page")).await.expect("no management authority required");
                assert_eq!(empty.count(), 0);
                assert!(empty.corrections().is_empty());
                let observer = database.connections(&database.user).recover().await.expect("independent SQL");
                let subject = observer.client().query_one("SELECT account.person_id FROM identity.sessions session JOIN identity.accounts account ON account.id = session.account_id WHERE session.id = $1", &[&target.session_id()]).await.expect("original person");
                let person_id: person::Id = subject.get(0);
                let mut expected = Vec::new();
                let mut previous_name = String::from("Target Person");
                let mut previous_number = "00000020000229007X";
                for index in 0..101 {
                    if index == 1 {
                        Box::pin(crate::access::grant::tests::admit(
                            &services,
                            &manager,
                            crate::access::Entry::Institution,
                            "13800000000",
                            crate::access::role::Kind::Default,
                        ))
                        .await;
                        Box::pin(services.number().allocate(
                            target.clone(),
                            Uuid::new_v4().to_string().parse().expect("allocation operation"),
                            serde_json::from_value(json!({"subtype":801})).expect("allocation input"),
                        )).await.expect("an independent number event between identity corrections");
                    }
                    let candidate = services.correction().candidate_by_person(&manager, person_id).await.expect("confirmed current version");
                    let name = format!("History {index}");
                    let id_number = if index % 2 == 0 { "000000200002290029" } else { "00000020000229007X" };
                    let receipt = Box::pin(services.correction().correct(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input(&candidate, &name, id_number))).await.expect("real correction");
                    expected.push((receipt.correction().id(), previous_name, previous_number, name.clone(), id_number));
                    previous_name = name;
                    previous_number = id_number;
                }
                expected.reverse();
                let rows = observer.client().query("SELECT id, sequence, corrected_at FROM identity.corrections WHERE person_id = $1 ORDER BY sequence DESC, id DESC", &[&person_id]).await.expect("independent committed order before read");
                assert_eq!(rows.len(), 101);
                assert_eq!(rows.iter().map(|row| row.get::<_, correction::Id>("id")).collect::<Vec<_>>(), expected.iter().map(|record| record.0).collect::<Vec<_>>());
                assert_eq!(rows[99].get::<_, i64>("sequence") - rows[100].get::<_, i64>("sequence"), 1, "number allocation does not advance the personal sequence");
                proxy.arm(Fault::DelayedStateQueries);
                let started = std::time::Instant::now();
                let page = services.correction().history(&target, Pagination::new(1, 100).expect("maximum page")).await.expect("complete page inside the read deadline");
                eprintln!("full self-correction history observation: {:?}; queries delayed by 8 ms: {}", started.elapsed(), proxy.delayed_queries());
                assert!(
                    (1..=10).contains(&proxy.delayed_queries()),
                    "a full page shares one bounded batch of fixed references instead of querying per correction"
                );
                assert_eq!(page.count(), 101);
                assert_eq!(page.corrections().len(), 100);
                for ((record, expected), row) in page.corrections().iter().zip(&expected).zip(&rows) {
                    assert_eq!(record.id(), expected.0);
                    assert_eq!(record.before().name().as_ref(), expected.1);
                    assert_eq!(record.before().suffix(), &expected.2[14..]);
                    assert_eq!(record.after().name().as_ref(), expected.3);
                    assert_eq!(record.after().suffix(), &expected.4[14..]);
                    assert_eq!(record.corrected_at(), row.get::<_, chrono::DateTime<chrono::Utc>>("corrected_at"));
                }
                let last = services.correction().history(&target, Pagination::new(2, 100).expect("second page")).await.expect("oldest correction");
                assert_eq!(last.count(), 101);
                assert_eq!(last.corrections().len(), 1);
                assert_eq!(last.corrections()[0].before().name().as_ref(), "Target Person");
                assert_eq!(last.corrections()[0].before().suffix(), "007X");
                assert_eq!(last.corrections()[0].id(), expected[100].0);
                let empty = services.correction().history(&target, Pagination::new(3, 100).expect("outside history")).await.expect("bounded empty page");
                assert_eq!(empty.count(), 101);
                assert!(empty.corrections().is_empty());
                let other = services.correction().history(&manager, Pagination::new(1, 100).expect("other person")).await.expect("administrator sees only own history here");
                assert_eq!(other.count(), 0);
                assert!(other.corrections().is_empty());
                let operator_id: person::Id = observer.client().query_one("SELECT account.person_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id WHERE session.id=$1", &[&manager.session_id()]).await.expect("actual operator person").get(0);
                proxy.arm(Fault::DelayedStateQueries);
                let managed = services.correction().history_by_person(&manager, person_id, Pagination::new(1, 100).expect("managed full page")).await.expect("bounded complete management page");
                assert!(
                    (1..=10).contains(&proxy.delayed_queries()),
                    "management authenticates the full count and the requested page with bounded reference batches"
                );
                assert_eq!(managed.count(), 101); assert_eq!(managed.corrections().len(), 100);
                for ((record, expected), row) in managed.corrections().iter().zip(&expected).zip(&rows) {
                    assert_eq!(record.id(), expected.0);
                    assert_eq!(record.before().name().as_ref(), expected.1);
                    assert_eq!(record.before().id_number().as_ref(), expected.2);
                    assert_eq!(record.after().name().as_ref(), expected.3);
                    assert_eq!(record.after().id_number().as_ref(), expected.4);
                    assert_eq!(record.operator().person_id(), operator_id);
                    assert_eq!(record.operator().name().as_ref(), "隔离测试");
                    assert_eq!(record.corrected_at(), row.get::<_, chrono::DateTime<chrono::Utc>>("corrected_at"));
                }
                let last = services.correction().history_by_person(&manager, person_id, Pagination::new(2, 100).expect("managed last page")).await.expect("oldest complete definition");
                assert_eq!(last.count(), 101); assert_eq!(last.corrections().len(), 1);
                assert_eq!(last.corrections()[0].before().name().as_ref(), "Target Person");
                assert_eq!(last.corrections()[0].before().id_number().as_ref(), "00000020000229007X");
                let outside = services.correction().history_by_person(&manager, person_id, Pagination::new(3, 100).expect("managed outside page")).await.expect("count survives an outside page");
                assert_eq!(outside.count(), 101); assert!(outside.corrections().is_empty());
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await; proxy.stop().await;
            })).await.expect("bounded complete history lifecycle");
        }));
    }

    #[test]
    fn management_history_uses_current_scope_and_retains_the_original_operator_names() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let manager = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let target = Box::pin(register_person(&database, &services, &mut supplier, "13700000000", "History Subject", "000000200002290029")).await;
                let selected = Box::pin(services.role().create(administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(json!({"name":"Correction managers","level":400,"permissions":["institution.corrections"]})).expect("role"))).await.expect("delegated correction role");
                let candidate = services.appointment().candidate(&administrator, "13800000000".parse().expect("mobile")).await.expect("manager confirmation");
                Box::pin(services.appointment().assign(administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"), appointment::tests::command(&candidate, &selected))).await.expect("real delegated manager");
                let manager_id = candidate.identity().person_id();
                let observer = database.connections(&database.user).recover().await.expect("independent SQL");
                let person_id: person::Id = observer.client().query_one("SELECT account.person_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id WHERE session.id=$1", &[&target.session_id()]).await.expect("target person").get(0);
                let administrator_id: person::Id = observer.client().query_one("SELECT account.person_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id WHERE session.id=$1", &[&administrator.session_id()]).await.expect("administrator person").get(0);
                let page = || Pagination::new(1, 20).expect("page");
                assert_eq!(services.correction().history_by_person(&manager, person_id, page()).await.expect("initial empty target history").count(), 0);
                for id in [manager_id, administrator_id, person::Id::allocate()] {
                    assert!(matches!(services.correction().history_by_person(&manager, id, page()).await, Err(Error::Absent)));
                }
                assert_eq!(services.correction().history_by_person(&target, person_id, page()).await.err().expect("self management still requires exact permission").failure(), crate::correction::Failure::Denied);
                let first = services.correction().candidate_by_person(&manager, person_id).await.expect("first confirmation");
                Box::pin(services.correction().correct(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input(&first, "First Subject", "000000200002290029"))).await.expect("first managed correction");
                let manager_profile = services.correction().candidate_by_person(&administrator, manager_id).await.expect("manager can be corrected by administrator");
                Box::pin(services.correction().correct(administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input(&manager_profile, "Renamed Manager", "00000020000229007X"))).await.expect("later operator name");
                let second = services.correction().candidate_by_person(&manager, person_id).await.expect("same valid session after a name change");
                Box::pin(services.correction().correct(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input(&second, "Second Subject", "000000200002290029"))).await.expect("second managed correction");
                let rows = observer.client().query("SELECT correction.id,correction.corrected_at,correction.operator_id,revision.person_id FROM identity.corrections correction JOIN identity.revisions revision ON revision.id=correction.operator_id WHERE correction.person_id=$1 ORDER BY correction.sequence DESC,correction.id DESC", &[&person_id]).await.expect("independent original operators before reads");
                assert_eq!(rows.len(), 2); assert_ne!(rows[0].get::<_, revision::Id>("operator_id"), rows[1].get::<_, revision::Id>("operator_id"));
                let list = services.correction().history_by_person(&administrator, person_id, page()).await.expect("current authority does not depend on being the original operator");
                assert_eq!(list.count(), 2);
                for ((record, row), name) in list.corrections().iter().zip(&rows).zip(["Renamed Manager", "Target Person"]) {
                    assert_eq!(record.id(), row.get("id")); assert_eq!(record.corrected_at(), row.get::<_, chrono::DateTime<chrono::Utc>>("corrected_at"));
                    assert_eq!(record.operator().person_id(), row.get("person_id"));
                    assert_eq!(record.operator().name().as_ref(), name);
                    assert_eq!(record.before().id_number().as_ref(), "000000200002290029");
                    assert_eq!(record.after().id_number().as_ref(), "000000200002290029");
                }
                let definition = Box::pin(services.role().create(administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(json!({"name":"Equal target","level":400,"permissions":[]})).expect("role"))).await.expect("equal target role");
                let confirmation = services.appointment().candidate(&administrator, "13700000000".parse().expect("mobile")).await.expect("target assignment confirmation");
                let input = serde_json::from_value(json!({"target":{"accountId":confirmation.identity().id(),"personId":person_id,"revisionId":confirmation.identity().revision_id(),"assignmentId":confirmation.assignment_id(),"mobile":"13700000000"},"role":{"id":definition.id(),"changeId":definition.change_id()}})).expect("exact target assignment");
                Box::pin(services.appointment().assign(administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input)).await.expect("equal target committed");
                assert!(matches!(services.correction().history_by_person(&manager, person_id, page()).await, Err(Error::Absent)), "past corrections do not grant continued access");
                let account = services.current().read(&target).await.expect("latest target confirmation");
                let cancelled = services.cancellation().cancel(
                    target,
                    Uuid::new_v4().to_string().parse().expect("cancellation operation"),
                    serde_json::from_value(json!({
                        "accountId": account.id(),
                        "personId": account.person_id(),
                        "revisionId": account.revision_id(),
                    })).expect("self cancellation confirmation"),
                ).await.expect("lawful cancellation preserves the original history");
                assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.accounts WHERE id=$2 AND person_id=$1 AND cancellation_id=$3 AND mobile IS NULL AND id_number IS NULL AND suspension_id IS NULL) AND (SELECT count(*) FROM identity.corrections WHERE person_id=$1)=2", &[&person_id, &account.id(), &cancelled.id()]).await.expect("persistent history survives the account tombstone").get::<_, bool>(0));
                let former = services.correction().history_by_person(&manager, person_id, page()).await.expect("no current target account requires no level comparison");
                assert_eq!(former.count(), 2); assert_eq!(former.corrections()[1].operator().name().as_ref(), "Target Person");
                let new_target = Box::pin(register_person(&database, &services, &mut supplier, "13700000000", "Different Person", "000000200002290029")).await;
                assert_eq!(services.correction().history(&new_target, page()).await.expect("same numbers do not inherit old personal history").count(), 0);
                assert_eq!(services.correction().history_by_person(&new_target, person_id, page()).await.err().expect("same numbers do not authorize management").failure(), crate::correction::Failure::Denied);
                assert_eq!(services.correction().history_by_person(&manager, person_id, page()).await.expect("original person remains separately readable").count(), 2);
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded management scope and identity verification");
        }));
    }

    #[test]
    fn management_history_keeps_authorization_and_the_complete_page_in_one_snapshot() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = crate::access::role::tests::start_snapshot(authentication::proxied(&database, &proxy), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let manager = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                Box::pin(register_person(&database, &services, &mut supplier, "13700000000", "Snapshot Subject", "000000200002290029")).await;
                let selected = Box::pin(services.role().create(administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(json!({"name":"History readers","level":400,"permissions":["institution.corrections"]})).expect("role"))).await.expect("correction role");
                let candidate = services.appointment().candidate(&administrator, "13800000000".parse().expect("mobile")).await.expect("manager candidate");
                Box::pin(services.appointment().assign(administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"), appointment::tests::command(&candidate, &selected))).await.expect("manager assigned");
                let candidate = services.correction().candidate_by_mobile(&manager, "13700000000".parse().expect("mobile")).await.expect("target");
                let person_id = candidate.target().person_id();
                Box::pin(services.correction().correct(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input(&candidate, "Snapshot First", "000000200002290029"))).await.expect("original correction");
                let page = || Pagination::new(1, 20).expect("page");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                proxy.arm(Fault::StalledCorrectionCount);
                let service = services.correction().clone(); let reader = manager.clone();
                let task = tokio::spawn(async move { service.history_by_person(&reader, person_id, page()).await });
                timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("decisive authorization snapshot established");
                let updated = Box::pin(services.role().update(administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"), selected.id(), serde_json::from_value(json!({"changeId":selected.change_id(),"name":"History readers","level":400,"permissions":[]})).expect("revoke exact permission"))).await.expect("current permission revoked");
                let next = services.correction().candidate_by_person(&administrator, person_id).await.expect("new administrator confirmation");
                let added = Box::pin(services.correction().correct(administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input(&next, "Snapshot Second", "000000200002290029"))).await.expect("concurrent correction");
                proxy.release();
                let old = timeout(Duration::from_secs(2), task).await.expect("old snapshot completes").expect("reader task");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.corrections WHERE person_id=$1", &[&person_id]).await.expect("two committed corrections").get::<_, i64>(0), 2);
                assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.entries WHERE change_id=$1 AND capability_id='institution.corrections')", &[&updated.change_id()]).await.expect("permission actually removed").get::<_, bool>(0));
                assert_eq!(old.err().expect("moved Person anchor retries current permission").failure(), crate::correction::Failure::Denied);
                assert_eq!(services.correction().history_by_person(&manager, person_id, page()).await.err().expect("later request loses authority").failure(), crate::correction::Failure::Denied);
                let latest = services.correction().history_by_person(&administrator, person_id, page()).await.expect("new authorized snapshot sees both");
                assert_eq!(latest.count(), 2); assert_eq!(latest.corrections()[0].id(), added.correction().id());
                // Deadline and finalization checks retain the production read budget.
                services.close(); services.drain().await.expect("snapshot services drained"); context.stop().await.expect("snapshot context stopped");
                let (context, services) = start(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                let unrelated = database.connections(&database.user).recover().await.expect("same-role unrelated backend");
                unrelated.client().batch_execute("BEGIN READ ONLY; SELECT 1").await.expect("another idle transaction must not identify the reader");
                for cancelled in [true, false] {
                    proxy.arm(Fault::StalledCorrectionPage);
                    let service = services.correction().clone(); let reader = administrator.clone();
                    let task = tokio::spawn(async move { service.history_by_person(&reader, person_id, page()).await });
                    let pid = timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("management page stopped after authenticated count");
                    if cancelled { task.abort(); assert!(task.await.err().expect("cancelled task").is_cancelled()); }
                    else {
                        let result = timeout(Duration::from_secs(4), task).await.expect("read deadline").expect("task");
                        let error = result.err().expect("stalled page fails");
                        assert!(matches!(error, Error::Timeout(_)), "actual read failure: {error:?}");
                    }
                    proxy.release();
                    timeout(Duration::from_secs(3), async { loop {
                        if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("backend lifetime").get::<_, bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("cancelled or failed page backend ends");
                    assert!(services.correction().registry.ready().is_ok());
                }
                unrelated.client().batch_execute("ROLLBACK").await.expect("unrelated backend survives failed reads");
                unrelated.terminate().await.expect("unrelated backend ended");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.corrections WHERE person_id=$1", &[&person_id]).await.expect("failed reads changed no facts").get::<_, i64>(0), 2);
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await; proxy.stop().await;
            })).await.expect("bounded management snapshot and read lifetime");
        }));
    }

    #[test]
    fn management_history_rejects_damaged_operator_fields_without_partial_disclosure() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (field, route) in [("name", "self"), ("id_number", "self"), ("name", "management"), ("id_number", "management")] {
                timeout(Duration::from_secs(15), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    for name in ["Earlier Subject", "Later Subject"] {
                        let candidate = services.correction().candidate_by_mobile(&manager, "13800000000".parse().expect("mobile")).await.expect("target");
                        Box::pin(services.correction().correct(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input(&candidate, name, "00000020000229007X"))).await.expect("real correction");
                    }
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    let person_id: person::Id = owner.client().query_one("SELECT account.person_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id WHERE session.id=$1", &[&target.session_id()]).await.expect("target person").get(0);
                    // Keep the caller's current identity healthy so both routes reach historical disclosure.
                    let operator_id = owner.client().query_one("SELECT operator_id FROM identity.corrections WHERE person_id=$1 ORDER BY sequence LIMIT 1", &[&person_id]).await.expect("historical operator").get(0);
                    let operator = revision::fetch(owner.client(), services.correction().roles.keys(), operator_id).await.expect("original operator identity");
                    rename_administrator(&services, operator.person_id(), operator.revision_id(), "Healthy Current Operator", operator.id_number().as_ref()).await;
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated operator field damage");
                    let sql = match field {
                        "name" => "UPDATE identity.revisions SET name=set_byte(name,octet_length(name)-1,get_byte(name,octet_length(name)-1)#1) WHERE id=(SELECT operator_id FROM identity.corrections WHERE person_id=$1 ORDER BY sequence LIMIT 1)",
                        _ => "UPDATE identity.revisions SET id_number=set_byte(id_number,octet_length(id_number)-1,get_byte(id_number,octet_length(id_number)-1)#1) WHERE id=(SELECT operator_id FROM identity.corrections WHERE person_id=$1 ORDER BY sequence LIMIT 1)",
                    };
                    assert_eq!(owner.client().execute(sql, &[&person_id]).await.expect("exact operator field changed"), 1);
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let snapshot = "SELECT jsonb_build_object('corrections',(SELECT jsonb_agg(to_jsonb(correction) ORDER BY sequence) FROM identity.corrections correction WHERE person_id=$1),'revisions',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM identity.revisions revision))::text";
                    let original: String = owner.client().query_one(snapshot, &[&person_id]).await.expect("facts before failed read").get(0);
                    let page = || Pagination::new(1, 20).expect("page");
                    let result = if route == "self" {
                        services.correction().history(&target, page()).await.map(|_| ())
                    } else {
                        services.correction().history_by_person(&manager, person_id, page()).await.map(|_| ())
                    };
                    let error = result.expect_err("fixed operator identity authenticates even when the projection does not disclose it");
                    assert!(matches!(error, Error::Fact(correction::Error::Revision(_))), "{field}: {error:?}");
                    assert_eq!(error.failure(), crate::correction::Failure::Unavailable); assert!(services.correction().registry.ready().is_err());
                    assert_eq!(owner.client().query_one(snapshot, &[&person_id]).await.expect("failed read preserves original damage and history").get::<_, String>(0), original);
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert_eq!(owner.client().query_one(snapshot, &[&person_id]).await.expect("drain does not change history").get::<_, String>(0), original);
                    owner.terminate().await.expect("owner ended");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded operator field verification");
            }
        }));
    }

    #[test]
    fn self_history_uses_one_authorized_snapshot_and_releases_failed_reads() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = crate::access::role::tests::start_snapshot(authentication::proxied(&database, &proxy), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let candidate = services.correction().candidate_by_mobile(&manager, "13800000000".parse().expect("mobile")).await.expect("first confirmation");
                Box::pin(services.correction().correct(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input(&candidate, "First Name", "00000020000229007X"))).await.expect("first correction");
                let person_id = candidate.target().person_id();
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let page = || Pagination::new(1, 20).expect("page");
                proxy.arm(Fault::StalledCorrectionCount);
                let service = services.correction().clone(); let actor = target.clone();
                let task = tokio::spawn(async move { service.history(&actor, page()).await });
                timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("snapshot established before count");
                let candidate = services.correction().candidate_by_person(&manager, person_id).await.expect("second confirmation");
                let added = Box::pin(services.correction().correct(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input(&candidate, "Second Name", "000000200002290029"))).await.expect("concurrent correction");
                let candidate = services.account().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("pause confirmation");
                let input = serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"})).expect("pause input");
                Box::pin(services.suspension().suspend(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), candidate.identity().id(), input)).await.expect("concurrent real suspension");
                proxy.release();
                let original = timeout(Duration::from_secs(2), task).await.expect("snapshot completes").expect("reader task");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.corrections WHERE person_id = $1", &[&person_id]).await.expect("two committed corrections").get::<_, i64>(0), 2);
                assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.revocations WHERE id = $1)", &[&target.session_id()]).await.expect("original session actually revoked").get::<_, bool>(0));
                assert!(matches!(original, Err(Error::Current(crate::identity::current::Error::Invalid))), "moved Person anchor retries the now revoked session");
                let latest = services.correction().history_by_person(&manager, person_id, page()).await.expect("fresh authorized snapshot");
                assert_eq!(latest.count(), 2); assert_eq!(latest.corrections()[0].id(), added.correction().id());
                assert!(matches!(services.correction().history(&target, page()).await, Err(Error::Current(crate::identity::current::Error::Invalid))));
                proxy.arm(Fault::RejectedRollback);
                let error = services.correction().history(&target, page()).await.err().expect("failed rejection cleanup");
                assert_eq!(error.failure(), crate::correction::Failure::Unavailable);
                assert!(services.correction().registry.ready().is_ok());

                // Deadline and finalization checks retain the production read budget.
                services.close(); services.drain().await.expect("snapshot services drained"); context.stop().await.expect("snapshot context stopped");
                let (context, services) = start(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                for cancelled in [true, false] {
                    proxy.arm(Fault::StalledCorrectionCount);
                    let service = services.correction().clone(); let actor = manager.clone();
                    let started = tokio::time::Instant::now();
                    let task = tokio::spawn(async move { service.history(&actor, page()).await });
                    let pid = timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual history query stopped");
                    if cancelled {
                        task.abort(); assert!(task.await.err().expect("cancelled reader").is_cancelled());
                    } else {
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                        proxy.arm(Fault::StalledCorrectionPage); proxy.release();
                        timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("page follows count under the same deadline");
                        let error = timeout(Duration::from_millis(2200), task).await.expect("one read deadline").expect("reader").err().expect("no partial page");
                        assert!(matches!(error, Error::Timeout(_)), "{error:?}");
                        assert!(started.elapsed() < Duration::from_millis(3900));
                    }
                    proxy.release();
                    timeout(Duration::from_secs(3), async { loop {
                        if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid = $1)", &[&pid]).await.expect("backend lifetime").get::<_, bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("cancelled or failed read connection actually ended");
                    assert!(services.correction().registry.ready().is_ok());
                }
                {
                    proxy.arm(Fault::StalledCorrectionCount);
                    let service = services.correction().clone(); let actor = manager.clone();
                    let task = tokio::spawn(async move { service.history(&actor, page()).await });
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("qualified snapshot before completion fault");
                    proxy.arm(Fault::LostReadCommit); proxy.release();
                    // Do not let this completion notification satisfy the next count wait.
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual completion fault intercepted");
                    let error = timeout(Duration::from_secs(4), task).await.expect("bounded read finalization").expect("reader").err().expect("failed finalization withholds the page");
                    assert_eq!(error.failure(), crate::correction::Failure::Unavailable);
                        let cause = match &error { Error::Combined { cause, .. } => cause.as_ref(), _ => &error };
                        assert!(matches!(cause, Error::End(crate::database::transaction::EndError::Commit(_))), "{error:?}");
                        assert!(services.correction().registry.ready().is_ok());
                    proxy.release();
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.corrections WHERE person_id = $1", &[&person_id]).await.expect("failed reads made no corrections").get::<_, i64>(0), 2);
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); let _ = context.stop().await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await; proxy.stop().await;
            })).await.expect("bounded snapshot and failure verification");
        }));
    }

    #[test]
    fn self_history_rejects_damaged_facts_without_returning_partial_or_false_empty_pages() {
        use crate::runtime::work;

        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["previous", "new", "operator", "source", "pointer", "previous_name", "previous_number", "name", "number", "no_change", "time", "sequence", "removed", "query"] {
                timeout(Duration::from_secs(15), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let candidate = services.correction().candidate_by_mobile(&manager, "13800000000".parse().expect("mobile")).await.expect("initial identity");
                    let person_id = candidate.target().person_id();
                    let first = Box::pin(services.correction().correct(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input(&candidate, "First Name", "00000020000229007X"))).await.expect("first correction");
                    let next = services.correction().candidate_by_person(&manager, person_id).await.expect("current identity");
                    Box::pin(services.correction().correct(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input(&next, "Second Name", "000000200002290029"))).await.expect("latest healthy correction");
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role = replica").await.expect("isolated permanent fact damage");
                    match case {
                        "previous" => { owner.client().execute("DELETE FROM identity.revisions WHERE id = $1", &[&candidate.target().revision_id()]).await.expect("missing initial identity"); }
                        "new" => { owner.client().execute("DELETE FROM identity.revisions WHERE id = $1", &[&first.correction().id()]).await.expect("missing corrected identity"); }
                        "operator" => { owner.client().execute("UPDATE identity.corrections SET operator_id = gen_random_uuid() WHERE id = $1", &[&first.correction().id()]).await.expect("missing undisclosed operator"); }
                        "source" => { owner.client().execute("UPDATE identity.revisions SET transaction_id = pg_current_xact_id() WHERE id = $1", &[&first.correction().id()]).await.expect("wrong revision source"); }
                        "pointer" => { owner.client().execute("UPDATE identity.persons SET revision_id = $1 WHERE id = $2", &[&candidate.target().revision_id(), &person_id]).await.expect("incorrect current pointer"); }
                        "previous_name" | "name" => {
                            let revision_id = if case == "previous_name" { candidate.target().revision_id() } else { first.correction().id().revision_id() };
                            owner.client().execute("UPDATE identity.revisions SET name = id_number WHERE id = $1", &[&revision_id]).await.expect("wrong name envelope binding");
                        }
                        "previous_number" | "number" => {
                            let revision_id = if case == "previous_number" { candidate.target().revision_id() } else { first.correction().id().revision_id() };
                            owner.client().execute("UPDATE identity.revisions SET id_number = set_byte(id_number, octet_length(id_number)-1, get_byte(id_number, octet_length(id_number)-1)#1) WHERE id = $1", &[&revision_id]).await.expect("number ciphertext damage");
                        }
                        "no_change" => {
                            revision::fixture_update(owner.client(), services.correction().roles.keys(), first.correction().id().revision_id(), person_id, candidate.name(), candidate.id_number()).await.expect("authenticated false historical change");
                        }
                        "time" => { owner.client().execute("UPDATE identity.corrections SET corrected_at = clock_timestamp() + interval '1 day' WHERE person_id = $1", &[&person_id]).await.expect("future history"); }
                        "sequence" => { owner.client().execute("UPDATE identity.persons SET sequence = sequence + 1 WHERE id = $1", &[&person_id]).await.expect("missing personal event"); }
                        "removed" => { owner.client().execute("DELETE FROM identity.corrections WHERE person_id = $1", &[&person_id]).await.expect("all corrections improperly removed"); }
                        "query" => { owner.client().batch_execute(&format!("REVOKE SELECT ON identity.changes FROM {}", database.user)).await.expect("ordinary read failure"); }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("isolated damage committed");
                    let snapshot = "SELECT jsonb_build_object('person',(SELECT to_jsonb(person) FROM identity.persons person WHERE id=$1),'corrections',(SELECT jsonb_agg(to_jsonb(correction) ORDER BY sequence) FROM identity.corrections correction WHERE person_id=$1),'revisions',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM identity.revisions revision WHERE person_id=$1))::text";
                    let before: String = owner.client().query_one(snapshot, &[&person_id]).await.expect("independent exact damaged facts").get(0);
                    let page = Pagination::new(if case == "sequence" { 100 } else { 1 }, 20).expect("requested page");
                    let error = services.correction().history(&target, page).await.err().expect("damage withholds the entire page");
                    assert_eq!(error.failure(), crate::correction::Failure::Unavailable, "{case}: {error:?}");
                    assert_eq!(work::Failure::unreliable(&error), case != "query", "{case}: {error:?}");
                    assert_eq!(services.correction().registry.ready().is_err(), case != "query");
                    if case == "query" {
                        assert!(matches!(&error, Error::Fact(correction::Error::History(crate::history::Error::Query(cause))) if cause.code() == Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)), "{error:?}");
                    }
                    assert_eq!(owner.client().query_one(snapshot, &[&person_id]).await.expect("failed read leaves facts untouched").get::<_, String>(0), before);
                    if case == "query" {
                        owner.client().batch_execute(&format!("GRANT SELECT ON identity.changes TO {}", database.user)).await.expect("restore fixture grant");
                        assert_eq!(services.correction().history(&target, page).await.expect("ordinary failure does not close admission").count(), 2);
                    }
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded historical damage case");
            }
        }));
    }

    #[test]
    fn online_completion_damage_withholds_history_and_startup() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["missing", "target", "input", "time", "seal", "wrong source", "dual source"] {
                timeout(Duration::from_secs(15), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let candidate = services.correction().candidate_by_mobile(&manager, "13800000000".parse().expect("mobile")).await.expect("candidate");
                    let person_id = candidate.target().person_id();
                    let operation: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
                    Box::pin(services.correction().correct(manager.clone(), operation, input(&candidate, "Online Damage", "00000020000229007X"))).await.expect("healthy correction");
                    let page = Pagination::new(2, 1).expect("empty page beyond the retained correction");
                    assert_eq!(services.correction().history(&target, page).await.expect("healthy off-page count").count(), 1);
                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role = replica").await.expect("isolated completion damage");
                    match case {
                        "missing" => { owner.client().execute("DELETE FROM identity.completions WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("missing online completion"); }
                        "target" => { owner.client().execute("UPDATE identity.completions SET target=gen_random_uuid()::text WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("wrong completion target"); }
                        "input" => { owner.client().execute("UPDATE identity.completions SET input=set_byte(input,0,get_byte(input,0)#1) WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("sealed input damage"); }
                        "time" => { owner.client().execute("UPDATE identity.completions SET created_at=created_at+interval '1 second' WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("wrong completion time"); }
                        "seal" => { owner.client().execute("UPDATE identity.completions SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("seal damage"); }
                        "wrong source" => { owner.client().execute("UPDATE identity.completions SET family='system_correction',subject='system' WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("wrong completion source"); }
                        "dual source" => {
                            // A second source is inserted with triggers disabled; startup and reads
                            // must still reject the contradiction from the retained rows.
                            let now = context.sample().await.expect("trusted completion time");
                            let operator = services.current().read(&manager).await.expect("operator");
                            let request = crate::kernel::completion::Request::system::<std::convert::Infallible>(
                                "correct", person_id.to_string(),
                                &serde_json::json!({"operator":{"mobile":"13900000000"}}),
                                &crate::kernel::Authorization {
                                    person_id: operator.person_id(),
                                    revision_id: operator.revision_id(),
                                    now,
                                },
                            ).expect("system completion request");
                            crate::kernel::completion::insert::<std::convert::Infallible>(
                                owner.client(), services.correction().app.keys(), correction::COMPLETIONS,
                                operation, &request, now, None,
                            ).await.expect("second completion source");
                        }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let error = services.correction().history(&target, page).await.err().expect("damaged completion also withholds an empty page");
                    assert_eq!(error.failure(), crate::correction::Failure::Unavailable, "{case}: {error:?}");
                    assert!(crate::runtime::work::Failure::unreliable(&error), "{case}: {error:?}");
                    assert!(services.correction().registry.ready().is_err(), "{case}");
                    let now = context.sample().await.expect("verification time");
                    correction::verify(owner.client(), services.correction().roles.keys(), now).await.expect_err("startup rejects a damaged online completion");
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM identity.corrections WHERE operation_id=$1", &[&operation]).await.expect("no write through failed history").get::<_, i64>(0), 1);
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    let restarted = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                    let result = Box::pin(crate::startup::Services::start(
                        crate::database::Barrier::begin(restarted.clone()).await.expect("restart barrier"),
                        authentication::inventory(authentication::keys()),
                        crate::startup::Settings::new(
                            4,
                            crate::identity::message::Limits::default(),
                            authentication::session_settings(),
                            crate::registration::Limits::default(),
                            authentication::avatar_settings(),
                        ).expect("settings"),
                        supplier.client(),
                        authentication::unused_client().await,
                    )).await;
                    assert!(result.is_err(), "{case}: actual startup rejects damaged online completion");
                    restarted.stop().await.expect("restart context stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded completion damage case");
            }
        }));
    }

    #[test]
    fn online_completion_damage_fails_replay_and_startup() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["missing", "target", "input", "time", "seal", "wrong source", "dual source"] {
                timeout(Duration::from_secs(15), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let candidate = services.correction().candidate_by_mobile(&manager, "13800000000".parse().expect("mobile")).await.expect("candidate");
                    let person_id = candidate.target().person_id();
                    let operation: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
                    Box::pin(services.correction().correct(manager.clone(), operation, input(&candidate, "Online Damage", "00000020000229007X"))).await.expect("healthy correction");
                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role = replica").await.expect("isolated completion damage");
                    match case {
                        "missing" => { owner.client().execute("DELETE FROM identity.completions WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("missing online completion"); }
                        "target" => { owner.client().execute("UPDATE identity.completions SET target=gen_random_uuid()::text WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("wrong completion target"); }
                        "input" => { owner.client().execute("UPDATE identity.completions SET input=set_byte(input,0,get_byte(input,0)#1) WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("sealed input damage"); }
                        "time" => { owner.client().execute("UPDATE identity.completions SET created_at=created_at+interval '1 second' WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("wrong completion time"); }
                        "seal" => { owner.client().execute("UPDATE identity.completions SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("seal damage"); }
                        "wrong source" => { owner.client().execute("UPDATE identity.completions SET family='system_correction',subject='system' WHERE family='correction' AND operation_id=$1", &[&operation]).await.expect("wrong completion source"); }
                        "dual source" => {
                            let now = context.sample().await.expect("trusted completion time");
                            let operator = services.current().read(&manager).await.expect("operator");
                            let request = crate::kernel::completion::Request::system::<std::convert::Infallible>(
                                "correct", person_id.to_string(),
                                &serde_json::json!({"operator":{"mobile":"13900000000"}}),
                                &crate::kernel::Authorization {
                                    person_id: operator.person_id(),
                                    revision_id: operator.revision_id(),
                                    now,
                                },
                            ).expect("system completion request");
                            crate::kernel::completion::insert::<std::convert::Infallible>(
                                owner.client(), services.correction().app.keys(), correction::COMPLETIONS,
                                operation, &request, now, None,
                            ).await.expect("second completion source");
                        }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let error = Box::pin(services.correction().correct(
                        manager.clone(), operation, input(&candidate, "Online Damage", "00000020000229007X")
                    )).await.err().expect("damaged completion cannot replay");
                    assert!(matches!(error, crate::kernel::Error::Corrupt(_)), "{case}: {error:?}");
                    assert!(services.correction().registry.ready().is_err(), "{case}");
                    let now = context.sample().await.expect("verification time");
                    correction::verify(owner.client(), services.correction().roles.keys(), now).await.expect_err("startup rejects a damaged online completion");
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM identity.corrections WHERE operation_id=$1", &[&operation]).await.expect("replay wrote nothing").get::<_, i64>(0), 1);
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded completion replay case");
            }
        }));
    }

    #[test]
    fn orphan_completions_block_both_correction_sources_and_startup() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (online_completion, online_command) in [(true, true), (true, false), (false, true), (false, false)] {
                timeout(Duration::from_secs(15), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let candidate = services.correction().candidate_by_mobile(&manager, "13800000000".parse().expect("mobile")).await.expect("candidate");
                    let operator = services.current().read(&manager).await.expect("operator");
                    let operation = Uuid::new_v4().to_string().parse().expect("operation");
                    let now = context.sample().await.expect("trusted time");
                    let authority = crate::kernel::Authorization {
                        person_id: operator.person_id(), revision_id: operator.revision_id(), now,
                    };
                    let online = input(&candidate, "Orphan Completion", "00000020000229007X");
                    let system = json!({
                        "operator": {"mobile": "13900000000"},
                        "target": {"personId": candidate.target().person_id(), "revisionId": candidate.target().revision_id()},
                        "name": "Orphan Completion", "idNumber": "00000020000229007X",
                    });
                    let request = if online_completion {
                        crate::kernel::completion::Request::new::<std::convert::Infallible>(
                            "correct", candidate.target().person_id().to_string(), &online, &authority,
                        )
                    } else {
                        crate::kernel::completion::Request::system::<std::convert::Infallible>(
                            "correct", candidate.target().person_id().to_string(),
                            &correction::Canonical {
                                operator: correction::CanonicalOperator { mobile: "13900000000" },
                                target: correction::CanonicalTarget {
                                    person_id: candidate.target().person_id(),
                                    revision_id: candidate.target().revision_id(),
                                },
                                name: &online.name, id_number: &online.id_number,
                            },
                            &authority,
                        )
                    }.expect("complete canonical input");
                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role = replica").await.expect("isolate an orphan without changing any identity");
                    let template = if online_completion { correction::ONLINE } else { correction::COMPLETIONS };
                    crate::kernel::completion::insert::<std::convert::Infallible>(
                        owner.client(), services.correction().app.keys(), template, operation, &request, now, None,
                    ).await.expect("authenticated orphan completion");
                    owner.client().batch_execute("COMMIT").await.expect("orphan retained");
                    assert!(crate::kernel::completion::original::<std::convert::Infallible>(
                        owner.client(), services.correction().app.keys(), template, operation,
                    ).await.expect("valid seal and canonical completion").is_some());
                    correction::verify(owner.client(), services.correction().roles.keys(), now).await.expect_err("startup detects the orphan in the reverse direction");
                    if online_command {
                        let error = Box::pin(services.correction().correct(manager, operation, online)).await.err().expect("online command rejects orphan");
                        assert!(matches!(error, crate::kernel::Error::Corrupt(_)), "{error:?}");
                    } else {
                        let error = Box::pin(correction::admin::run(
                            services.correction().app.clone(), Arc::clone(&services.correction().keys),
                            operation, serde_json::from_value(system).expect("system input"), false,
                        )).await.err().expect("system command rejects orphan");
                        assert!(matches!(error, crate::kernel::Error::Corrupt(_)), "{error:?}");
                    }
                    assert!(services.correction().registry.ready().is_err());
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM identity.corrections", &[]).await.expect("no correction was accepted").get::<_,i64>(0), 0);
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    supplier.stop().await;
                })).await.expect("bounded orphan case");
            }
        }));
    }

    #[test]
    fn online_correction_replay_survives_operator_rename_but_requires_its_live_session() {
        use crate::access::role::Update;
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let operator = register_person(&database, &services, &mut supplier, "13800000000", "Operator Person", "00000020000229007X").await;
                let target = register_person(&database, &services, &mut supplier, "13700000000", "Target Person", "000000200002290029").await;
                let operator_person = services.current().read(&operator).await.expect("operator identity").person_id();
                let definition = Box::pin(services.role().create(
                    administrator.clone(), Uuid::new_v4().to_string().parse().expect("role operation"),
                    serde_json::from_value(json!({
                        "name": "Online correction operators", "level": 100, "permissions": ["institution.corrections"]
                    })).expect("role input")
                )).await.expect("precise correction role");
                let selected = services.appointment()
                    .candidate(&administrator, "13800000000".parse().expect("operator mobile"))
                    .await.expect("operator confirmation");
                Box::pin(services.appointment().assign(
                    administrator.clone(), Uuid::new_v4().to_string().parse().expect("appointment operation"),
                    appointment::tests::command(&selected, &definition)
                )).await.expect("operator appointment");
                let candidate = services.correction()
                    .candidate_by_mobile(&operator, "13700000000".parse().expect("target mobile"))
                    .await.expect("manageable target");
                let person_id = candidate.target().person_id();
                assert_eq!(person_id, services.current().read(&target).await.expect("target identity").person_id());
                let original_operation: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
                let command = || input(&candidate, "Target Corrected", "000000200002290029");
                let original = Box::pin(services.correction().correct(operator.clone(), original_operation, command())).await.expect("original correction");
                assert!(original.created());
                let operator_candidate = services.correction()
                    .candidate_by_mobile(&administrator, "13800000000".parse().expect("operator mobile"))
                    .await.expect("operator confirmation after appointment");
                Box::pin(services.correction().correct(
                    administrator.clone(), Uuid::new_v4().to_string().parse().expect("rename operation"),
                    input(&operator_candidate, "Renamed Operator", "00000020000229007X")
                )).await.expect("operator renamed by a higher administrator");
                let replay = Box::pin(services.correction().correct(operator.clone(), original_operation, command())).await.expect("renamed operator replays its original");
                assert!(!replay.created());
                assert_eq!(replay.correction().id(), original.correction().id());
                assert_eq!(replay.correction().corrected_at(), original.correction().corrected_at());
                Box::pin(services.role().update(
                    administrator.clone(), Uuid::new_v4().to_string().parse().expect("role update operation"),
                    definition.id(), Update::new(
                        definition.change_id(),
                        serde_json::from_value(json!({
                            "name": "Online correction operators", "level": 100, "permissions": []
                        })).expect("removed permission")
                    )
                )).await.expect("correction permission removed");
                let error = Box::pin(services.correction().correct(operator.clone(), original_operation, command())).await.err().expect("lost permission rejects replay");
                assert!(matches!(error, crate::kernel::Error::Denied), "{error:?}");
                services.revocation().revoke(
                    operator.clone(), Uuid::new_v4().to_string().parse().expect("logout operation"),
                    crate::identity::revocation::Scope::Current
                ).await.expect("operator logout");
                let error = Box::pin(services.correction().correct(operator, original_operation, command())).await.err().expect("logged-out operator cannot replay");
                assert!(matches!(error, crate::kernel::Error::Unauthenticated), "{error:?}");
                let observer = database.connections(&database.user).recover().await.expect("independent owner");
                let facts = observer.client().query_one("SELECT count(*) FILTER (WHERE person_id=$1) AS target_count, count(*) FILTER (WHERE person_id=$2) AS operator_count FROM identity.corrections WHERE person_id=$1 OR person_id=$2", &[&person_id, &operator_person]).await.expect("independent correction count");
                assert_eq!((facts.get::<_, i64>("target_count"), facts.get::<_, i64>("operator_count")), (1, 1));
                observer.terminate().await.expect("independent owner ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("operator rename and session budget");
        }));
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ConfirmedOnline {
        target: ConfirmedTarget,
        name: crate::identity::person::name::Name,
        id_number: crate::identity::person::number::IdNumber,
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ConfirmedTarget {
        person_id: person::Id,
        revision_id: revision::Id,
    }

    /// A direct runtime correction commits only with its authenticated online completion.
    async fn append_online(
        client: &tokio_postgres::Client,
        context: &crate::runtime::Context,
        keys: crate::history::Keys<'_>,
        id: correction::Id,
        operator_id: revision::Id,
        operator_person: person::Id,
        input: ConfirmedOnline,
    ) -> Result<(), tokio_postgres::Error> {
        let person_id = input.target.person_id;
        let roots = crate::keys::tests::fixture();
        let sealed = revision::fixture_builder(
            crate::history::Keys {
                data: &roots.encryption,
                seals: &roots.seal,
            },
            id.revision_id(),
            person_id,
            &input.name,
            &input.id_number,
        )
        .expect("sealed revision");
        let now = context
            .sample()
            .await
            .expect("independent trusted time after begin");
        let operation_id: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
        let state = crate::history::inspect::<person::state::State>(client, keys, now)
            .await
            .expect("verified fixture person")
            .into_iter()
            .find(|state| state.id() == person_id)
            .expect("target state");
        let appointment = client.query_one(
            "SELECT holder.assignment_id FROM identity.accounts account JOIN access.holders holder ON holder.id=account.id WHERE account.person_id=$1",
            &[&person_id],
        ).await?.get(0);
        person::transition::correct(
            &state,
            input.target.revision_id,
            id.revision_id(),
            operator_id,
            Some(appointment),
            operation_id,
            now,
        )
        .expect("complete fixture transition")
        .write_offline(client, keys)
        .await
        .expect("sealed state and record");
        sealed
            .insert(client, revision::FixtureSource::Transaction)
            .await?;
        let request = crate::kernel::completion::Request::new::<std::convert::Infallible>(
            "correct",
            person_id.to_string(),
            &input,
            &crate::kernel::Authorization {
                person_id: operator_person,
                revision_id: operator_id,
                now,
            },
        )
        .expect("online completion request");
        crate::kernel::completion::insert::<std::convert::Infallible>(
            client,
            keys,
            correction::ONLINE,
            operation_id,
            &request,
            now,
            None,
        )
        .await
        .expect("online correction completion");
        Ok(())
    }

    #[test]
    fn runtime_corrections_require_complete_sources_and_terminal_pointers_after_early_checks() {
        use tokio_postgres::error::SqlState;
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let candidate = services.correction()
                    .candidate_by_mobile(&manager, "13800000000".parse().expect("mobile"))
                    .await.expect("candidate");
                let person_id = candidate.target().person_id();
                let previous = candidate.target().revision_id();
                let operator = services.current().read(&manager).await.expect("operator").revision_id();
                let operator_person = services.current().read(&manager).await.expect("operator person").person_id();
                let keys = services.correction().app.keys();
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                for early in [false, true] {
                    for case in ["complete", "missing revision", "missing correction", "missing completion", "missing state", "wrong before", "wrong pointer", "sequence gap", "unchanged version"] {
                        let session = database.connections(&database.user).recover().await.expect("runtime transaction");
                        let client = session.client();
                        client.batch_execute("BEGIN").await.expect("begin");
                        if early {
                            client.batch_execute("SET CONSTRAINTS ALL IMMEDIATE; SET CONSTRAINTS ALL DEFERRED").await.expect("an earlier check does not exempt later writes");
                        }
                        let id = correction::Id::allocate();
                        let operation: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
                        let now = context.sample().await.expect("trusted time");
                        let result: Result<(), tokio_postgres::Error> = async {
                            if case != "missing revision" {
                                revision::fixture_builder(keys, id.revision_id(), person_id,
                                    &"New Name".parse().expect("name"), candidate.id_number())
                                    .expect("sealed value").insert(client, revision::FixtureSource::Transaction).await?;
                            }
                            if case != "missing correction" {
                                let before = if case == "wrong before" { operator } else { previous };
                                client.execute(
                                    "INSERT INTO identity.corrections(id,person_id,operator_id,before_id,corrected_at,sequence,operation_id,appointment_id,seal,seal_key_id,seal_version) SELECT $1,person.id,$2,$3,$4,2,$5,holder.assignment_id,decode(repeat('00',32),'hex'),person.seal_key_id,1 FROM identity.persons person JOIN identity.accounts account ON account.person_id=person.id JOIN access.holders holder ON holder.id=account.id WHERE person.id=$6",
                                    &[&id,&operator,&before,&now,&operation,&person_id],
                                ).await?;
                            }
                            if case != "missing completion" {
                                let request = crate::kernel::completion::Request::new::<std::convert::Infallible>(
                                    "correct", person_id.to_string(),
                                    &ConfirmedOnline {
                                        target: ConfirmedTarget { person_id, revision_id: previous },
                                        name: "New Name".parse().expect("name"),
                                        id_number: candidate.id_number().clone(),
                                    },
                                    &crate::kernel::Authorization { person_id: operator_person, revision_id: operator, now },
                                ).expect("completion request");
                                crate::kernel::completion::insert::<std::convert::Infallible>(
                                    client, keys, correction::ONLINE, operation, &request, now, None,
                                ).await.expect("structurally complete receipt");
                            }
                            if case != "missing state" {
                                let revision = if case == "wrong pointer" { previous } else { id.revision_id() };
                                let sequence: i64 = if case == "sequence gap" { 3 } else { 2 };
                                let version: i64 = if case == "unchanged version" { 1 } else { 2 };
                                client.execute(
                                    "UPDATE identity.persons SET revision_id=$2,sequence=$3,version=$4,changed_at=$5,seal=decode(repeat('00',32),'hex') WHERE id=$1",
                                    &[&person_id,&revision,&sequence,&version,&now],
                                ).await?;
                            }
                            client.batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await
                        }.await;
                        if case == "complete" {
                            result.expect("all mandatory fields and same-transaction companions form a valid structural control");
                        } else {
                            let error = result.expect_err(case);
                            assert!(matches!(error.code(), Some(&SqlState::CHECK_VIOLATION) | Some(&SqlState::FOREIGN_KEY_VIOLATION)), "{case}/{early}: {error:?}");
                        }
                        // Dummy seals test SQL structure only; no fixture graph is committed.
                        client.batch_execute("ROLLBACK").await.expect("discard structural fixture");
                        session.terminate().await.expect("ended");
                        assert!(observer.client().query_one(
                            "SELECT person.revision_id=$2 AND person.sequence=1 AND NOT EXISTS(SELECT 1 FROM identity.corrections WHERE person_id=person.id) AND (SELECT count(*) FROM identity.revisions WHERE person_id=person.id)=1 FROM identity.persons person WHERE id=$1",
                            &[&person_id,&previous],
                        ).await.expect("unchanged independent graph").get::<_,bool>(0), "{case}/{early}");
                    }
                }
                let session = database.connections(&database.user).recover().await.expect("valid compound transaction");
                session.client().batch_execute("BEGIN").await.expect("begin");
                let confirmed = |revision_id, name: &str| ConfirmedOnline {
                    target: ConfirmedTarget { person_id, revision_id },
                    name: name.parse().expect("confirmed name"),
                    id_number: "00000020000229007X".parse().expect("confirmed number"),
                };
                let first = correction::Id::allocate();
                append_online(session.client(), &context, keys, first, operator, operator_person, confirmed(previous, "First Change"))
                    .await.expect("first complete correction");
                session.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE; SET CONSTRAINTS ALL DEFERRED")
                    .await.expect("first complete position");
                let second = correction::Id::allocate();
                append_online(session.client(), &context, keys, second, operator, operator_person, confirmed(first.revision_id(), "Second Change"))
                    .await.expect("second complete correction");
                session.client().batch_execute("COMMIT")
                    .await.expect("two complete corrections are not a one-event-per-transaction rule");
                session.terminate().await.expect("writer ended");
                let valid: bool = observer.client().query_one(
                    "SELECT person.revision_id=$2 AND person.sequence=3 AND (SELECT count(*) FROM identity.corrections WHERE person_id=person.id)=2 FROM identity.persons person WHERE id=$1",
                    &[&person_id, &second]
                ).await.expect("independent valid terminal graph").get(0);
                assert!(valid);
                let now = observer.client().query_one("SELECT clock_timestamp()", &[])
                    .await.expect("time").get(0);
                correction::verify(observer.client(), services.correction().roles.keys(), now)
                    .await.expect("all permanent lineage reconstructs");
                observer.terminate().await.expect("observer ended");
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
            })).await.expect("runtime protection budget");
        }));
    }

    #[test]
    fn correction_competitors_recheck_the_version_number_occupation_and_permission_after_waiting() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::{
            access::role,
            identity::authentication::{
                self,
                session::{
                    self,
                    tests::{CODE, message},
                },
            },
        };
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let manager = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let blocker = database.connections(&database.user).recover().await.expect("explicit blocker");
                let candidate = services.correction()
                    .candidate_by_mobile(&administrator, "13800000000".parse().expect("mobile"))
                    .await.expect("first target");
                let account_id: crate::identity::account::Id = observer.client().query_one(
                    "SELECT id FROM identity.accounts WHERE person_id=$1",
                    &[&candidate.target().person_id()]
                ).await.expect("target account").get(0);
                blocker.client().batch_execute("BEGIN").await.expect("hold target");
                blocker.client().execute("SELECT identity.lock_accounts($1,$1)", &[&account_id])
                    .await.expect("target exclusive lock");
                let mut workers = Vec::new();
                for name in ["First Winner", "Second Winner"] {
                    let service = services.correction().clone();
                    let actor = administrator.clone();
                    let input = input(&candidate, name, "00000020000229007X");
                    workers.push(tokio::spawn(async move {
                        service.correct(actor, Uuid::new_v4().to_string().parse().expect("operation"), input).await
                    }));
                }
                timeout(Duration::from_secs(3), async {
                    loop {
                        if observer.client().query_one(
                            "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND query = 'SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE' AND wait_event_type='Lock' AND cardinality(pg_blocking_pids(pid))>0",
                            &[]
                        ).await.expect("two real target waiters").get::<_, i64>(0) == 2 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("both callers wait before release");
                blocker.client().batch_execute("COMMIT").await.expect("release target");
                let mut succeeded = 0;
                let mut changed = 0;
                for worker in workers {
                    match worker.await.expect("worker") {
                        Ok(value) => {
                            assert!(value.created());
                            succeeded += 1;
                        },
                        Err(crate::kernel::Error::Rejected(super::Rejection::TargetChanged)) => changed += 1,
                        Err(error) => panic!("unexpected competitor: {error:?}")
                    }
                }
                assert_eq!((succeeded, changed), (1, 1));
                assert_eq!(
                    observer.client().query_one(
                        "SELECT count(*) FROM identity.corrections WHERE person_id=$1",
                        &[&candidate.target().person_id()]
                    ).await.expect("one committed definition before next scenario").get::<_, i64>(0),
                    1
                );
                let message_id = message(&database, services.authentication(), &mut supplier, "13700000000").await;
                let session::Receipt::Registration(credential) = Box::pin(
                    services.authentication().create_session(session::tests::input(
                        Uuid::new_v4(), message_id, "13700000000", CODE
                    ))
                ).await.expect("third person R1") else {
                    panic!("new registration")
                };
                Box::pin(services.registration().register(crate::registration::Input::new(
                    Uuid::new_v4().to_string().parse().expect("operation"),
                    credential.id(), credential.token().text().parse().expect("proof"),
                    "Third Person".parse().expect("name"),
                    "000000200002290029".parse().expect("number")
                ))).await.expect("third person R2");
                let first = services.correction()
                    .candidate_by_mobile(&administrator, "13800000000".parse().expect("mobile"))
                    .await.expect("first confirmation");
                let second = services.correction()
                    .candidate_by_mobile(&administrator, "13700000000".parse().expect("mobile"))
                    .await.expect("second confirmation");
                blocker.client().batch_execute("BEGIN").await.expect("hold number");
                crate::database::transaction::object(
                    blocker.client(), b"identity/id_number", b"000000210003010016"
                ).await.expect("same stable-number lock as R2");
                let mut workers = Vec::new();
                for candidate in [&first, &second] {
                    let service = services.correction().clone();
                    let actor = administrator.clone();
                    let command = input(candidate, "Number Winner", "000000210003010016");
                    workers.push(tokio::spawn(async move {
                        service.correct(actor, Uuid::new_v4().to_string().parse().expect("operation"), command).await
                    }));
                }
                timeout(Duration::from_secs(3), async {
                    loop {
                        if observer.client().query_one(
                            "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock(%' AND cardinality(pg_blocking_pids(pid))>0",
                            &[]
                        ).await.expect("two distinct target transactions wait on number").get::<_, i64>(0) == 2 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("both number contenders wait");
                blocker.client().batch_execute("COMMIT").await.expect("release number");
                let mut succeeded = 0;
                let mut occupied = 0;
                for worker in workers {
                    match worker.await.expect("worker") {
                        Ok(value) => {
                            assert!(value.created());
                            succeeded += 1;
                        },
                        Err(crate::kernel::Error::Rejected(super::Rejection::IdNumberTaken)) => occupied += 1,
                        Err(error) => panic!("unexpected number competitor: {error:?}")
                    }
                }
                assert_eq!((succeeded, occupied), (1, 1));
                assert_eq!(
                    observer.client().query_one(
                        "SELECT count(*) FROM identity.accounts WHERE id_number=$1",
                        &[&keys().number.hash(&"000000210003010016".parse().expect("stable future number")).expect("hash")]
                    ).await.expect("independent number ownership").get::<_, i64>(0),
                    1
                );
                let confirmation = services.correction()
                    .candidate_by_mobile(&administrator, "13800000000".parse().expect("mobile"))
                    .await.expect("same-operation confirmation");
                let same_operation: operation::Id = Uuid::new_v4().to_string().parse().expect("same operation");
                proxy.arm(Fault::DelayedCommit);
                let service = services.correction().clone();
                let actor = administrator.clone();
                let command = input(&confirmation, "Same Original", confirmation.id_number().as_ref());
                let original = tokio::spawn(async move {
                    service.correct(actor, same_operation, command).await
                });
                timeout(Duration::from_secs(3), proxy.intercepted())
                    .await.expect("first same operation holds its pending commit");
                let mut repeated = Box::pin(services.correction().correct(
                    administrator.clone(), same_operation,
                    input(&confirmation, "Same Original", confirmation.id_number().as_ref())
                ));
                std::future::poll_fn(|context| {
                    assert!(std::future::Future::poll(repeated.as_mut(), context).is_pending());
                    std::task::Poll::Ready(())
                }).await;
                assert_eq!(
                    observer.client().query_one(
                        "SELECT count(*) FROM identity.corrections WHERE operation_id=$1",
                        &[&same_operation]
                    ).await.expect("pending original not yet committed").get::<_, i64>(0),
                    0
                );
                proxy.release();
                let first_result = original.await.expect("original worker").expect("original completion");
                let repeated = repeated.await.expect("concurrent same input receives permanent original");
                assert!(first_result.created());
                assert!(!repeated.created());
                assert_eq!(repeated.correction().id(), first_result.correction().id());
                assert_eq!(
                    observer.client().query_one(
                        "SELECT count(*) FROM identity.corrections WHERE operation_id=$1",
                        &[&same_operation]
                    ).await.expect("only one permanent original").get::<_, i64>(0),
                    1
                );
                let confirmation = services.correction()
                    .candidate_by_mobile(&administrator, "13800000000".parse().expect("mobile"))
                    .await.expect("R2 competition target");
                let free_number = if confirmation.id_number().as_ref() == "000000210003010016" {
                    "00000020000229007X"
                } else {
                    "000000200002290029"
                };
                let message_id = message(&database, services.authentication(), &mut supplier, "13600000000").await;
                let session::Receipt::Registration(credential) = Box::pin(
                    services.authentication().create_session(session::tests::input(
                        Uuid::new_v4(), message_id, "13600000000", CODE
                    ))
                ).await.expect("competing R2 credential") else {
                    panic!("new R2")
                };
                blocker.client().batch_execute("BEGIN").await.expect("hold R2 shared number");
                crate::database::transaction::object(
                    blocker.client(), b"identity/id_number", free_number.as_bytes()
                ).await.expect("same number boundary");
                let service = services.correction().clone();
                let actor = administrator.clone();
                let command = input(&confirmation, "R2 Competition", free_number);
                let correcting = tokio::spawn(async move {
                    service.correct(actor, Uuid::new_v4().to_string().parse().expect("operation"), command).await
                });
                let service = services.registration().clone();
                let command = crate::registration::Input::new(
                    Uuid::new_v4().to_string().parse().expect("R2 operation"),
                    credential.id(), credential.token().text().parse().expect("proof"),
                    "Competing Registration".parse().expect("name"),
                    free_number.parse().expect("number")
                );
                let registering = tokio::spawn(async move {
                    service.register(command).await
                });
                timeout(Duration::from_secs(3), async {
                    loop {
                        if observer.client().query_one(
                            "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock(%' AND cardinality(pg_blocking_pids(pid))>0",
                            &[]
                        ).await.expect("R2 and correction both hold their own prerequisites and wait on shared number").get::<_, i64>(0) == 2 {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("two family number waiters");
                blocker.client().batch_execute("COMMIT").await.expect("release R2 competition");
                let correction_won = match (
                    correcting.await.expect("correction worker"),
                    registering.await.expect("registration worker")
                ) {
                    (Ok(receipt), Err(crate::registration::Error::IdNumberTaken)) => {
                        assert!(receipt.created());
                        true
                    },
                    (Err(crate::kernel::Error::Rejected(super::Rejection::IdNumberTaken)), Ok(_)) => false,
                    (correction, registration) => panic!(
                        "one family must own number: {:?}/{:?}",
                        correction.err(), registration.err()
                    )
                };
                assert_eq!(
                    observer.client().query_one(
                        "SELECT count(*) FROM identity.accounts WHERE id_number=$1",
                        &[&keys().number.hash(&free_number.parse().expect("number")).expect("hash")]
                    ).await.expect("one current owner independently observed before replay").get::<_, i64>(0),
                    1
                );
                let definition = Box::pin(services.role().create(
                    administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"),
                    serde_json::from_value(json!({
                        "name": "Correction managers", "level": 100, "permissions": ["institution.corrections"]
                    })).expect("role input")
                )).await.expect("precise correction role");
                let selected = services.appointment()
                    .candidate(&administrator, "13800000000".parse().expect("mobile"))
                    .await.expect("manager confirmation");
                Box::pin(services.appointment().assign(
                    administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"),
                    appointment::tests::command(&selected, &definition)
                )).await.expect("manager appointment");
                let candidate = services.correction()
                    .candidate_by_mobile(&manager, "13700000000".parse().expect("mobile"))
                    .await.expect("manageable target");
                assert!(matches!(
                    services.correction().candidate_by_mobile(&manager, "13900000000".parse().expect("administrator mobile")).await,
                    Err(super::Error::Absent)
                ), "an online manager cannot select the higher administrator");
                proxy.arm(Fault::DelayedCommit);
                let service = services.correction().clone();
                let acting = manager.clone();
                let command = input(&candidate, "Before Permission Change", candidate.id_number().as_ref());
                let correcting = tokio::spawn(async move {
                    service.correct(acting, Uuid::new_v4().to_string().parse().expect("operation"), command).await
                });
                let pid = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("actual correction holds its commit");
                let service = services.role().clone();
                let acting = administrator.clone();
                let role_id = definition.id();
                let change_id = definition.change_id();
                let updating = tokio::spawn(async move {
                    service.update(acting, Uuid::new_v4().to_string().parse().expect("operation"), role_id,
                        role::Update::new(change_id, serde_json::from_value(json!({
                            "name":"Correction managers renamed","level":100,"permissions":["institution.corrections"]
                        })).expect("retained permission"))
                    ).await
                });
                timeout(Duration::from_secs(3), async {
                    loop {
                        if observer.client().query_one(
                            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock(%' AND $1=ANY(pg_blocking_pids(pid)))",
                            &[&pid],
                        ).await.expect("role mutation waits behind the actual correction").get::<_,bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("scope mutation reaches the shared permission boundary");
                proxy.release();
                correcting.await.expect("correction task").expect("correction accepted under the retained permission");
                let definition = updating.await.expect("role task").expect("role change after correction");
                let candidate = services.correction().candidate_by_mobile(&manager, "13700000000".parse().expect("mobile"))
                    .await.expect("new confirmed revision");
                let snapshot = "SELECT person.revision_id, person.sequence AS person_sequence, account.id_number, (SELECT count(*) FROM identity.corrections WHERE person_id=person.id) AS corrections, (SELECT count(*) FROM identity.revisions WHERE person_id=person.id) AS revisions FROM identity.persons person JOIN identity.accounts account ON account.person_id=person.id WHERE person.id=$1";
                let before = observer.client().query_one(snapshot,&[&candidate.target().person_id()]).await.expect("before permission loss");
                proxy.arm(Fault::DelayedCommit);
                let service = services.role().clone();
                let acting = administrator.clone();
                let change_id = definition.change_id();
                let updating = tokio::spawn(async move {
                    service.update(acting, Uuid::new_v4().to_string().parse().expect("operation"), role_id,
                        role::Update::new(change_id, serde_json::from_value(json!({
                            "name":"Correction managers renamed","level":100,"permissions":[]
                        })).expect("withdraw permission"))
                    ).await
                });
                let pid = timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("role mutation holds scope exclusive");
                let service = services.correction().clone();
                let command = input(&candidate,"Denied After Wait",candidate.id_number().as_ref());
                let waiting = tokio::spawn(async move {
                    service.correct(manager,Uuid::new_v4().to_string().parse().expect("operation"),command).await
                });
                timeout(Duration::from_secs(3),async {
                    loop {
                        if observer.client().query_one(
                            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock_shared%' AND $1=ANY(pg_blocking_pids(pid)))",
                            &[&pid],
                        ).await.expect("correction waits behind the actual permission withdrawal").get::<_,bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("correction reaches the permission boundary");
                proxy.release();
                updating.await.expect("role task").expect("permission withdrawn");
                assert!(matches!(waiting.await.expect("correction task"),Err(crate::kernel::Error::Denied)));
                let after = observer.client().query_one(snapshot,&[&candidate.target().person_id()]).await.expect("after rejected correction");
                assert_eq!(before.get::<_,revision::Id>("revision_id"),after.get("revision_id"));
                assert_eq!(before.get::<_,Vec<u8>>("id_number"),after.get::<_,Vec<u8>>("id_number"));
                for column in ["person_sequence","corrections","revisions"] {
                    assert_eq!(before.get::<_,i64>(column),after.get::<_,i64>(column),"{column}");
                }
                assert_eq!(
                    observer.client().query_one("SELECT count(*) FROM identity.corrections", &[])
                        .await.expect("exact successful corrections after all rejected races").get::<_, i64>(0),
                    4 + i64::from(correction_won)
                );
                blocker.terminate().await.expect("blocker ended");
                observer.terminate().await.expect("observer ended");
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
                proxy.stop().await;
            })).await.expect("correction concurrency budget");
        }));
    }

    #[test]
    fn correction_reconstruction_rejects_damaged_lineage_without_reclassifying_query_errors() {
        use crate::runtime::work;
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["previous", "new", "operator", "source", "pointer", "name", "number", "no_change", "time", "query"] {
                timeout(Duration::from_secs(15), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let candidate = services.correction()
                        .candidate_by_mobile(&manager, "13800000000".parse().expect("mobile"))
                        .await.expect("original confirmation");
                    let operation = Uuid::new_v4().to_string().parse().expect("operation");
                    let first = Box::pin(services.correction().correct(
                        manager.clone(), operation, input(&candidate, "First Corrected", "00000020000229007X")
                    )).await.expect("first correction");
                    let next = services.correction().candidate_by_person(&manager, candidate.target().person_id())
                        .await.expect("later confirmation");
                    Box::pin(services.correction().correct(
                        manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"),
                        input(&next, "Second Corrected", "00000020000229007X")
                    )).await.expect("later correction keeps original lineage");
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica")
                        .await.expect("deliberate persistent damage");
                    match case {
                        "previous" => {
                            owner.client().execute(
                                "DELETE FROM identity.revisions WHERE id=$1",
                                &[&candidate.target().revision_id()]
                            ).await.expect("missing unique initial version");
                        }
                        "new" => {
                            owner.client().execute(
                                "DELETE FROM identity.revisions WHERE id=$1",
                                &[&first.correction().id()]
                            ).await.expect("missing original new version");
                        }
                        "operator" => {
                            owner.client().execute(
                                "UPDATE identity.corrections SET operator_id=gen_random_uuid() WHERE id=$1",
                                &[&first.correction().id()]
                            ).await.expect("missing fixed actor relation");
                        }
                        "source" => {
                            owner.client().execute(
                                "UPDATE identity.revisions SET transaction_id=pg_current_xact_id() WHERE id=$1",
                                &[&first.correction().id()]
                            ).await.expect("wrong creation source");
                        }
                        "pointer" => {
                            owner.client().execute(
                                "UPDATE identity.persons SET revision_id=$1 WHERE id=$2",
                                &[&candidate.target().revision_id(), &candidate.target().person_id()]
                            ).await.expect("terminal pointer reverted");
                        }
                        "name" => {
                            owner.client().execute(
                                "UPDATE identity.revisions SET name=id_number WHERE id=$1",
                                &[&first.correction().id()]
                            ).await.expect("wrong field binding");
                        }
                        "number" => {
                            owner.client().execute(
                                "UPDATE identity.revisions SET id_number=set_byte(id_number,octet_length(id_number)-1,get_byte(id_number,octet_length(id_number)-1)#1) WHERE id=$1",
                                &[&first.correction().id()]
                            ).await.expect("number authentication damage");
                        }
                        "no_change" => {
                            revision::fixture_update(owner.client(), services.correction().roles.keys(), first.correction().id().revision_id(), candidate.target().person_id(), candidate.name(), candidate.id_number()).await.expect("authenticated empty original definition");
                        }
                        "time" => {
                            owner.client().execute(
                                "UPDATE identity.corrections SET corrected_at=clock_timestamp()+interval '1 day' WHERE id=$1",
                                &[&first.correction().id()]
                            ).await.expect("future original fact");
                        }
                        "query" => {
                            owner.client().batch_execute(&format!(
                                "REVOKE SELECT ON identity.changes FROM {}", database.user
                            )).await.expect("ordinary query denial");
                        }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("isolated damage committed");
                    let snapshot = "SELECT jsonb_build_object('person',(SELECT to_jsonb(person) FROM identity.persons person WHERE id=$1),'corrections',(SELECT jsonb_agg(to_jsonb(correction) ORDER BY sequence) FROM identity.corrections correction WHERE person_id=$1),'revisions',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM identity.revisions revision WHERE person_id=$1))::text";
                    let before: String = owner.client().query_one(snapshot, &[&candidate.target().person_id()])
                        .await.expect("independent damage before replay").get(0);
                    let replay = Box::pin(services.correction().correct(
                        manager.clone(), operation, input(&candidate, "First Corrected", "00000020000229007X")
                    )).await;
                    if case == "pointer" {
                        assert_eq!(replay.expect("an original result does not depend on today's target pointer").correction().id(), first.correction().id());
                        services.ready().await.expect("original recovery does not inspect unrelated current state");
                        person::verify(owner.client(), services.app().keys(), context.sample().await.expect("time"))
                            .await.expect_err("startup rejects the damaged current Person state");
                    } else {
                        let error = replay.err().expect("damaged necessary evidence is not an original success");
                        assert_eq!(work::Failure::unreliable(&error), case != "query", "{case}: {error:?}");
                        assert_eq!(services.correction().registry.ready().is_err(), case != "query");
                        if case == "query" {
                            assert!(matches!(&error, crate::kernel::Error::Unavailable(fault) if matches!(fault.kind(), crate::kernel::FaultKind::Correction(correction::Error::History(crate::history::Error::Query(cause))) if cause.code() == Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE))), "{error:?}");
                        } else {
                            assert!(error.is_corrupt(), "{case}: {error:?}");
                        }
                    }
                    assert_eq!(
                        owner.client().query_one(snapshot, &[&candidate.target().person_id()])
                            .await.expect("no replay writes").get::<_, String>(0),
                        before
                    );
                    if !matches!(case, "query" | "pointer") {
                        let now = context.sample().await.expect("trusted verification time");
                        let error = crate::identity::verification::verify(
                            owner.client(), &keys().encryption, &keys().mobile, &keys().number,
                            &authentication::inventory(keys()).seal, now
                        ).await.expect_err("startup owner rejects same permanent damage");
                        assert!(!format!("{error:?}").contains("First Corrected"), "diagnostics do not disclose historical names");
                    }
                    owner.terminate().await.expect("owner ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    supplier.stop().await;
                })).await.expect("bounded corruption case");
            }
        }));
    }

    #[test]
    fn corrections_without_accounts_still_check_occupation_before_name_only_changes() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let original = services.correction()
                    .candidate_by_mobile(&manager, "13800000000".parse().expect("mobile"))
                    .await.expect("original target");
                let person_id = original.target().person_id();
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                let account = services.current().read(&target).await.expect("current target");
                let cancellation = Box::pin(services.cancellation().cancel(
                    target,
                    Uuid::new_v4().to_string().parse().expect("cancellation operation"),
                    serde_json::from_value(serde_json::json!({
                        "accountId": account.id(), "personId": person_id,
                        "revisionId": account.revision_id(),
                    })).expect("confirmed self cancellation"),
                )).await.expect("complete cancellation retains the permanent identity");
                let candidate = services.correction().candidate_by_person(&manager, person_id)
                    .await.expect("permanent person remains manageable");
                let operation: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
                let first = Box::pin(services.correction().correct(
                    manager.clone(), operation, input(&candidate, "Former Target", "00000020000229007X")
                )).await.expect("unoccupied name-only correction without account");
                Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let row = owner.client().query_one(
                    "SELECT EXISTS(SELECT 1 FROM identity.accounts WHERE person_id=$1 AND id=$3 AND cancellation_id=$4 AND mobile IS NULL AND id_number IS NULL) AS cancelled,(SELECT count(*) FROM identity.corrections WHERE person_id=$1) AS corrections,(SELECT count(*) FROM identity.accounts WHERE id_number=$2 AND person_id<>$1) AS occupied",
                    &[&person_id, &keys().number.hash(&"00000020000229007X".parse().expect("number")).expect("hash"), &account.id(), &cancellation.id()]
                ).await.expect("independent current occupation before retry");
                assert!(row.get::<_, bool>("cancelled"));
                assert_eq!(row.get::<_, i64>("corrections"), 1);
                assert_eq!(row.get::<_, i64>("occupied"), 1);
                let current = services.correction().candidate_by_person(&manager, person_id)
                    .await.expect("current former identity");
                let error = Box::pin(services.correction().correct(
                    manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"),
                    input(&current, "Another Name", "00000020000229007X")
                )).await.err().expect("old number is now occupied");
                assert!(matches!(error, crate::kernel::Error::Rejected(super::Rejection::IdNumberTaken)));
                assert_eq!(
                    owner.client().query_one(
                        "SELECT count(*) FROM identity.corrections WHERE person_id=$1", &[&person_id]
                    ).await.expect("rejection has no new facts").get::<_, i64>(0),
                    1
                );
                let replay = Box::pin(services.correction().correct(
                    manager.clone(), operation, input(&candidate, "Former Target", "00000020000229007X")
                )).await.expect("original success precedes later occupation");
                assert!(!replay.created());
                assert_eq!(replay.correction().id(), first.correction().id());
                Box::pin(services.correction().correct(
                    manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"),
                    input(&current, "Another Name", "000000200002290029")
                )).await.expect("free number succeeds without creating an account");
                let now = owner.client().query_one("SELECT clock_timestamp()", &[])
                    .await.expect("time").get(0);
                correction::verify(owner.client(), services.correction().roles.keys(), now)
                    .await.expect("permanent sources remain complete");
                owner.terminate().await.expect("owner ended");
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
            })).await.expect("former account correction budget");
        }));
    }

    #[test]
    fn corrections_preserve_permanent_lineage_and_change_only_current_real_name() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let before = services.correction()
                    .candidate_by_mobile(&manager, "13800000000".parse().expect("mobile"))
                    .await.expect("full candidate");
                let person_id = before.target().person_id();
                assert_eq!(before.name().as_ref(), "Target Person");
                assert_eq!(
                    serde_json::to_value(before.id_number()).expect("borrowed string serialization"),
                    json!("00000020000229007X")
                );
                let observer = database.connections(&database.user).recover().await.expect("independent SQL");
                let sql = "SELECT person.revision_id, person.sequence AS person_sequence, encode(account.id_number,'hex') AS hash, holder.assignment_id, (SELECT count(*) FROM identity.sessions WHERE account_id=account.id) AS sessions, (SELECT count(*) FROM identity.handoffs WHERE id IN (SELECT id FROM identity.sessions WHERE account_id=account.id)) AS handoffs, (SELECT count(*) FROM identity.corrections WHERE person_id=person.id) AS corrections FROM identity.persons person JOIN identity.accounts account ON account.person_id=person.id JOIN access.holders holder ON holder.id=account.id WHERE person.id=$1";
                let initial = observer.client().query_one(sql, &[&person_id])
                    .await.expect("initial independent state");
                let operation: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
                let first = Box::pin(services.correction().correct(
                    manager.clone(), operation, input(&before, "  Re\u{301}vised Person  ", "00000020000229007x")
                )).await.expect("name correction");
                assert!(first.created());
                let deletion = observer.client().execute(
                    "DELETE FROM identity.completions WHERE family='correction' AND operation_id=$1",
                    &[&operation],
                ).await.expect_err("runtime cannot delete a permanent online completion");
                assert_eq!(deletion.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                let update = observer.client().execute(
                    "UPDATE identity.completions SET created_at=created_at WHERE family='correction' AND operation_id=$1",
                    &[&operation],
                ).await.expect_err("runtime cannot update a permanent online completion");
                assert_eq!(update.code(), Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE));
                let after = observer.client().query_one(sql, &[&person_id])
                    .await.expect("committed facts before replay");
                assert_eq!(after.get::<_, i64>("person_sequence"), 2);
                assert_eq!(after.get::<_, i64>("corrections"), 1);
                assert_eq!(after.get::<_, String>("hash"), initial.get::<_, String>("hash"));
                for column in ["sessions", "handoffs"] {
                    assert_eq!(after.get::<_, i64>(column), initial.get::<_, i64>(column));
                }
                assert_eq!(after.get::<_, Uuid>("assignment_id"), initial.get::<_, Uuid>("assignment_id"));
                assert_ne!(after.get::<_, Uuid>("revision_id"), initial.get::<_, Uuid>("revision_id"));
                let current = services.current().read(&target).await.expect("old session sees corrected current identity");
                assert_eq!(current.name().as_ref(), "Révised Person");
                let original = Box::pin(services.correction().correct(
                    manager.clone(), operation, input(&before, "Révised Person", "00000020000229007X")
                )).await.expect("original canonical result");
                assert!(!original.created());
                assert_eq!(original.correction().id(), first.correction().id());
                assert_eq!(original.correction().corrected_at(), first.correction().corrected_at());
                let next = services.correction().candidate_by_person(&manager, person_id)
                    .await.expect("same person route");
                assert_eq!(next.target().revision_id(), current.revision_id());
                let second = Box::pin(services.correction().correct(
                    manager.clone(), Uuid::new_v4().to_string().parse().expect("second operation"),
                    input(&next, "Révised Person", "000000200002290029")
                )).await.expect("number correction");
                assert!(second.created());
                let final_state = observer.client().query_one(sql, &[&person_id])
                    .await.expect("both permanent changes before old replay");
                assert_eq!(final_state.get::<_, i64>("person_sequence"), 3);
                assert_eq!(final_state.get::<_, i64>("corrections"), 2);
                assert_ne!(final_state.get::<_, String>("hash"), initial.get::<_, String>("hash"));
                let old = Box::pin(services.correction().correct(
                    manager.clone(), operation, input(&before, "Révised Person", "00000020000229007X")
                )).await.expect("old permanent result after later correction");
                assert!(!old.created());
                assert_eq!(old.correction().id(), first.correction().id());
                let mismatch = Box::pin(services.correction().correct(
                    manager.clone(), operation, input(&next, "Révised Person", "00000020000229007X")
                )).await;
                assert!(matches!(mismatch, Err(crate::kernel::Error::Mismatch)), "{:?}", mismatch.err());
                let stale = Box::pin(services.correction().correct(
                    manager.clone(), Uuid::new_v4().to_string().parse().expect("stale operation"),
                    input(&before, "Stale Version", "00000020000229007X")
                )).await;
                assert!(matches!(stale, Err(crate::kernel::Error::Rejected(super::Rejection::TargetChanged))), "{:?}", stale.err());
                observer.terminate().await.expect("observer ended");
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let read = services.current().read(&target).await.expect("restart authenticates unchanged old session");
                assert_eq!(read.suffix(), "0029");
                services.close();
                services.drain().await.expect("restart drained");
                context.stop().await.expect("restart stopped");
                assert!(
                    timeout(Duration::from_millis(30), supplier.request()).await.is_err(),
                    "no suppliers through corrections, replay and restart"
                );
                supplier.stop().await;
            })).await.expect("correction lifecycle budget");
        }));
    }
}
