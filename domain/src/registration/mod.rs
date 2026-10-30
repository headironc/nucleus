//! R2 composes Identity and Access in one recoverable account-creation transaction.

mod database;
mod error;
mod retention;

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use platform::cryptography::{self, Encoded, hmac};
use tokio::{
    sync::{Mutex, watch},
    time::{Instant, timeout, timeout_at},
};
use tokio_postgres::{Client, error::SqlState};

pub use crate::registration::{
    error::{Error, Failure},
    retention::Retention,
};
use crate::{
    access::{appointment, holder, role},
    database::{
        transaction::{self, Completion, Known},
        verification,
    },
    identity::{
        account,
        keys::Keys,
        mobile::Mobile,
        operation,
        person::{name::Name, number::IdNumber},
        registration, session,
    },
    kernel::App,
    runtime::{
        Context,
        registry::{Family, Registry},
        work::{self, Work},
    },
};

const BUDGET: Duration = Duration::from_secs(10);

pub(crate) async fn verify(
    client: &Client,
    app: &App,
    keys: &Keys,
    settings: &session::settings::Settings,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    database::verify_all(client, app, keys, settings, now).await
}

pub(crate) async fn ready(client: &Client, app: &App, now: DateTime<Utc>) -> Result<(), Error> {
    let roles = role::Reader::new(app.clone());
    verification::Stage::RegistrationRole
        .batch(BUDGET, role::fetch_default_institution(client, &roles, now))
        .await??;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    hourly: usize,
}

impl Limits {
    pub fn new(hourly: usize) -> Result<Self, LimitsError> {
        if !(1..=5).contains(&hourly) {
            return Err(LimitsError);
        }
        Ok(Self { hourly })
    }

    pub fn hourly(&self) -> usize {
        self.hourly
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self { hourly: 5 }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("registration hourly limit must be between 1 and 5")]
pub struct LimitsError;

pub struct Input {
    operation_id: operation::Id,
    credential_id: registration::Id,
    token: registration::Token,
    name: Name,
    id_number: IdNumber,
    transport: &'static str,
}

impl Input {
    pub fn new(
        operation_id: operation::Id,
        credential_id: registration::Id,
        token: registration::Token,
        name: Name,
        id_number: IdNumber,
    ) -> Self {
        Self {
            operation_id,
            credential_id,
            token,
            name,
            id_number,
            transport: "cookie",
        }
    }

    fn encoded(&self) -> Result<Encoded, cryptography::Error> {
        Encoded::new(self)
    }
}

impl cryptography::Input<8> for Input {
    fn fields(&self) -> [&[u8]; 8] {
        [
            b"nucleus/identity/registration/v1",
            b"POST",
            b"/registrations",
            self.operation_id.as_bytes(),
            self.transport.as_bytes(),
            self.credential_id.as_bytes(),
            self.name.as_ref().as_bytes(),
            self.id_number.as_ref().as_bytes(),
        ]
    }
}

/// Original proof and source checked in one read-only snapshot before HTTP applies Origin.
pub struct Prepared {
    input: Input,
    origin: session::Origin,
}

impl Prepared {
    pub fn origin(&self) -> &session::Origin {
        &self.origin
    }
}

/// Constructed only after committed creation or independently verified original handoff.
pub struct Receipt {
    id: session::Id,
    token: session::Token,
    expires_at: DateTime<Utc>,
    created: bool,
}

impl Receipt {
    pub fn id(&self) -> session::Id {
        self.id
    }
    pub fn token(&self) -> &session::Token {
        &self.token
    }
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
    pub fn created(&self) -> bool {
        self.created
    }
}

#[derive(Clone)]
pub struct Service(Arc<Inner>);

impl Service {
    pub fn retention(&self) -> Retention {
        Retention::new(self.clone())
    }

    /// Move a disposable issuance graph before its next request, with no concurrent work.
    /// The fixture supplies its owner connection; production identities cannot bypass triggers.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn age_for_test(
        &self,
        client: &mut Client,
        elapsed: chrono::TimeDelta,
    ) -> Result<(), Error> {
        let transaction = client.transaction().await?;
        transaction
            .batch_execute("SET LOCAL session_replication_role = replica")
            .await?;
        let app = &self.0.app;
        let now = transaction
            .client()
            .query_one("SELECT clock_timestamp()", &[])
            .await?
            .try_get(0)?;
        let (accounts, persons) =
            account::record::age(transaction.client(), app.keys(), now, elapsed).await?;
        let sessions = session::state::age(transaction.client(), app.keys(), now, elapsed).await?;
        let holders = holder::age_registrations(transaction.client(), app.keys(), elapsed).await?;
        let interval = format!(
            "{} microseconds",
            elapsed
                .num_microseconds()
                .ok_or(crate::history::Error::Integrity(
                    "fixture age fits the stored timestamp precision",
                ))?,
        );
        for sql in [
            "UPDATE identity.challenges SET created_at=created_at-$1::text::interval,expires_at=expires_at-$1::text::interval,solved_at=solved_at-$1::text::interval,finished_at=finished_at-$1::text::interval",
            "UPDATE identity.messages SET created_at=created_at-$1::text::interval,expires_at=expires_at-$1::text::interval,finished_at=finished_at-$1::text::interval",
            "UPDATE identity.claims SET created_at=created_at-$1::text::interval",
            "UPDATE identity.checks SET expires_at=expires_at-$1::text::interval",
            "UPDATE identity.authentications SET finished_at=finished_at-$1::text::interval",
            "UPDATE identity.handoffs SET expires_at=expires_at-$1::text::interval",
            "UPDATE identity.attempts SET accepted_at=accepted_at-$1::text::interval",
        ] {
            transaction.execute(sql, &[&interval]).await?;
        }
        transaction.commit().await?;
        for state in persons {
            crate::history::Seed::anchor(app.anchors(), &state)?;
        }
        for state in accounts {
            crate::history::Seed::anchor(app.anchors(), &state)?;
        }
        for state in sessions {
            crate::history::Seed::anchor(app.anchors(), &state)?;
        }
        for state in holders {
            crate::history::Seed::anchor(app.anchors(), &state)?;
        }
        Ok(())
    }

    pub(crate) fn assemble(
        app: App,
        keys: Arc<Keys>,
        settings: session::settings::Settings,
        limits: Limits,
    ) -> Self {
        Self(Arc::new(Inner {
            context: app.context().clone(),
            registry: Arc::clone(app.registry()),
            work: Arc::clone(app.work()),
            roles: role::Reader::new(app.clone()),
            app,
            keys,
            settings,
            limits,
            pending: Mutex::new(HashMap::new()),
        }))
    }

    pub async fn prepare(&self, mut input: Input) -> Result<Prepared, Error> {
        let result = self.0.preflight(&mut input).await;
        match result {
            Ok(origin) => Ok(Prepared { input, origin }),
            Err(error) => {
                if work::Failure::unreliable(&error) {
                    self.0.registry.close();
                }
                work::Failure::report(&error, input.operation_id);
                Err(error)
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn register_input_for_test(&self, input: Input) -> Result<Receipt, Error> {
        self.register(self.prepare(input).await?).await
    }

    pub async fn register(&self, prepared: Prepared) -> Result<Receipt, Error> {
        let Prepared { input, .. } = prepared;
        loop {
            self.0.work.ready(&self.0.registry).await?;
            {
                let mut pending = self.0.pending.lock().await;
                pending.retain(|_, request| request.completion.has_changed().is_ok());
                if let Some(request) = pending.get(&input.operation_id) {
                    let mut completion = request.subscribe(&input, self.0.keys.input.active())?;
                    drop(pending);
                    let _ = completion.changed().await;
                    continue;
                }
            }
            let encoded = input.encoded()?;
            let key = self.0.keys.input.active();
            let mut pending = self.0.pending.lock().await;
            pending.retain(|_, request| request.completion.has_changed().is_ok());
            if let Some(request) = pending.get(&input.operation_id) {
                let mut completion = request.subscribe(&input, key)?;
                drop(pending);
                let _ = completion.changed().await;
                continue;
            }
            let permit = self
                .0
                .registry
                .try_acquire_for(Family::Register, input.operation_id)
                .await?;
            pending.insert(
                input.operation_id,
                Pending {
                    proof: input.token.0.hash(),
                    hash: key.sign(&encoded),
                    completion: permit.completion(),
                },
            );
            drop(pending);
            let service = Arc::clone(&self.0);
            return self
                .0
                .work
                .run_reserved(
                    Arc::clone(&self.0.registry),
                    Family::Register,
                    input.operation_id,
                    permit,
                    async move { service.execute(&input).await },
                )
                .await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn verify(&self, client: &Client, now: DateTime<Utc>) -> Result<(), Error> {
        verify(client, &self.0.app, &self.0.keys, &self.0.settings, now).await
    }
}

struct Inner {
    app: App,
    context: Context,
    roles: role::Reader,
    keys: Arc<Keys>,
    settings: session::settings::Settings,
    limits: Limits,
    registry: Arc<Registry>,
    work: Arc<Work>,
    pending: Mutex<HashMap<operation::Id, Pending>>,
}

struct Creation {
    account_id: account::Id,
    created_at: DateTime<Utc>,
    pending: crate::history::Pending,
}

/// A verified request binding lasts exactly as long as its shared work reservation.
struct Pending {
    proof: [u8; 32],
    hash: hmac::Tag,
    completion: watch::Receiver<()>,
}

impl Pending {
    fn subscribe(
        &self,
        input: &Input,
        key: &crate::identity::keys::request::Key,
    ) -> Result<watch::Receiver<()>, Error> {
        if !input.token.0.matches(&self.proof) {
            return Err(Error::Proof);
        }
        if !key.verify(&input.encoded()?, self.hash.as_ref()) {
            return Err(Error::Mismatch);
        }
        Ok(self.completion.clone())
    }
}

enum Outcome {
    Session {
        id: session::Id,
        token: session::Token,
        expires_at: DateTime<Utc>,
        created: bool,
    },
    MobileTaken,
    IdNumberTaken,
    Refused(Error),
}

impl Outcome {
    fn deliver(self) -> Result<Receipt, Error> {
        match self {
            Self::Session {
                id,
                token,
                expires_at,
                created,
            } => Ok(Receipt {
                id,
                token,
                expires_at,
                created,
            }),
            Self::MobileTaken => Err(Error::MobileTaken),
            Self::IdNumberTaken => Err(Error::IdNumberTaken),
            Self::Refused(error) => Err(error),
        }
    }
}

struct Admission<'a> {
    id: registration::attempt::Id,
    credential: &'a database::Credential,
    mobile: &'a Mobile,
    accepted_at: DateTime<Utc>,
}

impl Inner {
    async fn preflight(&self, input: &mut Input) -> Result<session::Origin, Error> {
        let lease = self.context.connections().lease().await?;
        let result = timeout(BUDGET, async {
            // One read-only snapshot cannot miss both a just-consumed credential and its receipt.
            // No original-operation lock is taken before an in-flight proof has been checked.
            lease.client().batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; SET LOCAL statement_timeout = '10s'; SET LOCAL idle_in_transaction_session_timeout = '10s'").await?;
            let origin = match database::find_by_operation(lease.client(), input.operation_id).await? {
                Some(database::Original::Receipt(receipt)) => {
                    input.transport = receipt.origin().transport();
                    receipt.verify(lease.client(), self.app.keys(), &self.settings, input).await?;
                    receipt.origin().clone()
                },
                Some(database::Original::Attempt(attempt)) => {
                    input.transport = attempt.origin().transport();
                    attempt.verify(&self.keys.input, &self.settings, input)?;
                    attempt.origin().clone()
                },
                None => {
                    let credential = database::find_credential(lease.client(), input.credential_id).await?.ok_or(Error::Verification)?;
                    input.transport = credential.source.origin.transport();
                    credential.verify(&self.keys.credential, &self.keys.encryption, &self.keys.mobile, &self.settings, input)?;
                    credential.source.origin.clone()
                }
            };
            lease.client().batch_execute("COMMIT").await?;
            Ok::<_, Error>(origin)
        }).await;
        match result {
            Ok(Ok(origin)) => {
                lease.release();
                Ok(origin)
            }
            Ok(Err(error)) if error.rejected() => Err(Self::rollback(lease, error).await.0),
            Ok(Err(error)) => Err(error.combine(lease.discard().await)),
            Err(error) => Err(Error::from(error).combine(lease.discard().await)),
        }
    }

    async fn rollback(lease: platform::database::Lease, cause: Error) -> (Error, bool) {
        let (completion, cleanup) = transaction::rollback(
            cause,
            Instant::now() + BUDGET,
            lease.client().batch_execute("ROLLBACK"),
        )
        .await
        .recycle(lease)
        .await;
        let confirmed = completion.end.is_ok();
        let error = match completion.end {
            Ok(()) => completion.value,
            Err(rollback) => Error::Rollback {
                cause: Box::new(completion.value),
                rollback,
            },
        };
        (error.combine(cleanup), confirmed)
    }

    async fn now(&self, client: &tokio_postgres::Client) -> Result<DateTime<Utc>, Error> {
        Ok(self.context.now(client).await?)
    }

    async fn execute(&self, input: &Input) -> Result<Receipt, Error> {
        // A recovered completion is new only when this work created that exact session.
        let mut session = None;
        let mut creation = None;
        let cause = match self.context.connections().lease().await {
            Ok(lease) => {
                let deadline = Instant::now() + BUDGET;
                match timeout_at(
                    deadline,
                    self.write(lease.client(), input, &mut session, &mut creation),
                )
                .await
                {
                    Ok(Ok(outcome)) => {
                        let completion = transaction::commit(
                            outcome,
                            deadline,
                            lease.client().batch_execute("COMMIT"),
                        )
                        .await;
                        let (completion, cleanup) = completion.recycle(lease).await;
                        match completion {
                            Completion::Known(Known { value, end }) => {
                                if let Some(creation) = creation.take() {
                                    creation.pending.committed()?;
                                }
                                if end.is_err() || cleanup.is_err() {
                                    tracing::error!(operation_id = %input.operation_id, stage = "registration_completion", kind = "unavailable", outcome = "committed", commit = ?end, cleanup = ?cleanup);
                                }
                                return value.deliver();
                            }
                            Completion::Pending { error, .. } => {
                                Error::from(error).combine(cleanup)
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        if error.rejected() {
                            let (error, confirmed) = Self::rollback(lease, error).await;
                            if let Some(creation) = creation.take() {
                                if confirmed {
                                    creation.pending.rolled_back()?;
                                } else {
                                    creation.pending.uncertain();
                                }
                            }
                            return Err(error);
                        }
                        error.combine(lease.discard().await)
                    }
                    Err(error) => Error::from(error).combine(lease.discard().await),
                }
            }
            Err(error) => error.into(),
        };
        match self.recover(input, &mut creation).await {
            Ok((
                Known {
                    value: Some(mut result),
                    end: commit,
                },
                termination,
            )) => {
                if let Outcome::Session { id, created, .. } = &mut result {
                    *created = session == Some(*id);
                }
                let unreliable = work::Failure::unreliable(&cause);
                if unreliable {
                    self.registry.close();
                }
                if unreliable || commit.is_err() || termination.is_err() {
                    let outcome = if matches!(result, Outcome::Refused(_)) {
                        "known_refusal"
                    } else {
                        "committed"
                    };
                    tracing::error!(operation_id = %input.operation_id, stage = "registration_completion", kind = "unavailable", outcome, error = ?cause, commit = ?commit, termination = ?termination);
                }
                result.deliver()
            }
            Ok((
                Known {
                    value: None,
                    end: commit,
                },
                termination,
            )) => Err(Error::NotCommitted {
                cause: Box::new(Error::Readback {
                    cause: Box::new(cause),
                    commit,
                    termination,
                }),
            }),
            Err(recovery) => {
                if let Some(creation) = creation {
                    creation.pending.uncertain();
                }
                Err(Error::Unknown {
                    cause: Box::new(cause),
                    recovery: Box::new(recovery),
                })
            }
        }
    }

    async fn write(
        &self,
        client: &Client,
        input: &Input,
        session: &mut Option<session::Id>,
        creation: &mut Option<Creation>,
    ) -> Result<transaction::Outcome<Outcome>, Error> {
        for _ in 0..3 {
            transaction::begin(
                &self.context,
                client,
                b"registration/operation",
                input.operation_id,
            )
            .await?;
            if let Some(original) = database::find_by_operation(client, input.operation_id).await? {
                let result = self.deliver(client, input, original).await?;
                return Ok(transaction::Outcome::Known(result));
            }
            crate::access::role::lock_institution(client).await?;
            // Discovery does not lock the credential before the mobile shared with R1 replacement.
            let credential = database::find_credential(client, input.credential_id)
                .await?
                .ok_or(Error::Verification)?;
            let mobile = credential.verify(
                &self.keys.credential,
                &self.keys.encryption,
                &self.keys.mobile,
                &self.settings,
                input,
            )?;
            let mobile_hash = self.keys.mobile.hash(&mobile)?;
            let number_hash = self.keys.number.hash(&input.id_number)?;
            let accounts =
                account::state::discover_occupancies(client, &mobile_hash, &number_hash).await?;
            for account in &accounts {
                database::lock_account(client, *account).await?;
            }
            transaction::object(client, b"identity/mobile", mobile.as_bytes()).await?;
            transaction::object(
                client,
                b"identity/id_number",
                input.id_number.as_ref().as_bytes(),
            )
            .await?;
            if account::state::discover_occupancies(client, &mobile_hash, &number_hash).await?
                != accounts
            {
                client.batch_execute("ROLLBACK").await?;
                continue;
            }
            database::lock_credential(client, input.credential_id).await?;
            let credential = database::find_credential(client, input.credential_id)
                .await?
                .ok_or(Error::Verification)?;
            let current_mobile = credential.verify(
                &self.keys.credential,
                &self.keys.encryption,
                &self.keys.mobile,
                &self.settings,
                input,
            )?;
            if current_mobile.as_bytes() != mobile.as_bytes() {
                return Err(Error::Facts {
                    rule: "credential mobile is immutable",
                });
            }
            let now = self.now(client).await?;
            credential.validate(now)?;
            input
                .id_number
                .validate_day(database::day(client, now).await?)?;
            database::quota(client, &mobile_hash, now, self.limits.hourly()).await?;
            let admission = Admission {
                id: registration::attempt::Id::allocate(),
                credential: &credential,
                mobile: &mobile,
                accepted_at: now,
            };
            let result = self.create(client, input, &admission, creation).await?;
            if let Outcome::Session { id, .. } = &result {
                *session = Some(*id);
            }
            return Ok(transaction::Outcome::Pending(result));
        }
        Err(crate::runtime::work::Error::Busy.into())
    }

    async fn recover(
        &self,
        input: &Input,
        creation: &mut Option<Creation>,
    ) -> Result<
        (
            Known<Option<Outcome>>,
            Result<(), platform::database::Error>,
        ),
        Error,
    > {
        let session = self.context.connections().recover().await?;
        let deadline = Instant::now() + BUDGET;
        let result = async {
            let result = timeout_at(deadline, async {
                transaction::begin(
                    &self.context,
                    session.client(),
                    b"registration/operation",
                    input.operation_id,
                )
                .await?;
                if let Some(candidate) = creation.as_ref() {
                    let committed = account::record::created(
                        session.client(),
                        self.app.keys(),
                        self.app.anchors(),
                        candidate.account_id,
                        input.operation_id,
                        account::record::Source::Registration,
                        candidate.created_at,
                    )
                    .await?;
                    let holder = holder::created(
                        session.client(),
                        self.app.keys(),
                        self.app.anchors(),
                        candidate.account_id,
                        input.operation_id,
                        holder::Source::Registration,
                        candidate.created_at,
                    )
                    .await?;
                    if committed != holder {
                        return Err(Error::Facts {
                            rule: "account and first holder creation share one commit",
                        });
                    }
                    if let Some(candidate) = creation.take() {
                        if committed {
                            candidate.pending.committed()?;
                        } else {
                            candidate.pending.rolled_back()?;
                        }
                    }
                }
                let result = match database::find_by_operation(session.client(), input.operation_id)
                    .await?
                {
                    Some(original) => match self.deliver(session.client(), input, original).await {
                        Ok(result) => Some(result),
                        Err(error) if error.rejected() => Some(Outcome::Refused(error)),
                        Err(error) => return Err(error),
                    },
                    None => None,
                };
                Ok::<_, Error>(result)
            })
            .await??;
            // A complete locked read establishes the result before this transaction ends.
            Ok::<_, Error>(
                transaction::confirm(result, deadline, session.client().batch_execute("COMMIT"))
                    .await,
            )
        }
        .await;
        match result {
            Ok(result) => Ok(result.terminate(session).await),
            Err(error) => Err(error.combine(session.terminate().await)),
        }
    }

    async fn deliver(
        &self,
        client: &Client,
        input: &Input,
        original: database::Original,
    ) -> Result<Outcome, Error> {
        match original {
            database::Original::Attempt(attempt) => {
                attempt.verify(&self.keys.input, &self.settings, input)?;
                attempt.validate(
                    &self.keys.input,
                    &self.keys.encryption,
                    &self.keys.mobile,
                    &self.settings,
                    self.now(client).await?,
                )?;
                match attempt.status {
                    registration::attempt::Status::Registered => Err(Error::Verification),
                    registration::attempt::Status::MobileTaken => Ok(Outcome::MobileTaken),
                    registration::attempt::Status::IdNumberTaken => Ok(Outcome::IdNumberTaken),
                }
            }
            database::Original::Receipt(receipt) => {
                receipt
                    .verify(client, self.app.keys(), &self.settings, input)
                    .await?;
                database::lock_account(client, receipt.account_id).await?;
                let now = self.now(client).await?;
                let receipt = database::find_receipt_by_operation(client, input.operation_id)
                    .await?
                    .ok_or(Error::Verification)?;
                receipt
                    .verify(client, self.app.keys(), &self.settings, input)
                    .await?;
                let account = receipt
                    .validate(
                        client,
                        &self.app,
                        crate::history::Read::Shared,
                        &self.settings,
                        now,
                    )
                    .await?;
                let session = session::find_by_receipt(
                    client,
                    &self.app,
                    crate::history::Read::Shared,
                    receipt.id,
                    now,
                )
                .await?
                .ok_or(Error::Verification)?;
                receipt.validate_session(&session, &account)?;
                let token = session::handoff::deliver_origin(
                    client,
                    &session,
                    &self.keys.credential,
                    receipt.origin(),
                    now,
                )
                .await?;
                if !session.available(client, &self.app, now).await? {
                    return Err(Error::Verification);
                }
                Ok(Outcome::Session {
                    id: session.id(),
                    token,
                    expires_at: session.expires_at(),
                    created: false,
                })
            }
        }
    }

    async fn create(
        &self,
        client: &Client,
        input: &Input,
        admission: &Admission<'_>,
        creation: &mut Option<Creation>,
    ) -> Result<Outcome, Error> {
        let mobile = account::state::located(
            client,
            self.app.keys(),
            self.app.anchors(),
            crate::history::Read::Locked,
            account::state::Location::Mobile(&self.keys.mobile.hash(admission.mobile)?),
            admission.accepted_at,
        )
        .await?;
        let number = account::state::located(
            client,
            self.app.keys(),
            self.app.anchors(),
            crate::history::Read::Locked,
            account::state::Location::Number(&self.keys.number.hash(&input.id_number)?),
            admission.accepted_at,
        )
        .await?;
        let occupied = if mobile.is_some() {
            Some(registration::attempt::Status::MobileTaken)
        } else if number.is_some() {
            Some(registration::attempt::Status::IdNumberTaken)
        } else {
            None
        };
        let status = match occupied {
            Some(status) => status,
            None => {
                let identity = account::Creation::allocate();
                let prepared = identity.prepare(
                    account::Input {
                        mobile: admission.mobile,
                        name: &input.name,
                        id_number: &input.id_number,
                    },
                    account::Keys {
                        mobile: &self.keys.mobile,
                        encryption: &self.keys.encryption,
                        number: &self.keys.number,
                    },
                    account::record::Source::Registration,
                    input.operation_id,
                    admission.accepted_at,
                )?;
                client.batch_execute("SAVEPOINT creation").await?;
                let mut candidate = None;
                match prepared
                    .write(client, self.app.keys(), self.app.anchors(), &mut candidate)
                    .await
                    .map_err(Error::from)
                {
                    Ok(()) => {
                        *creation = Some(Creation {
                            account_id: identity.account_id,
                            created_at: admission.accepted_at,
                            pending: candidate.ok_or(Error::Facts {
                                rule: "account creation registers its pending candidates",
                            })?,
                        });
                        // Record pairing uses the top-level transaction identity across this
                        // savepoint, whose only purpose is precise uniqueness arbitration.
                        client.batch_execute("RELEASE SAVEPOINT creation").await?;
                        return self
                            .create_account(client, input, admission, identity, creation)
                            .await;
                    }
                    Err(error) => {
                        if let Some(pending) = candidate {
                            *creation = Some(Creation {
                                account_id: identity.account_id,
                                created_at: admission.accepted_at,
                                pending,
                            });
                        }
                        let status = match &error {
                            Error::Account(account::Error::Database(query))
                                if query.is_constraint(
                                    &SqlState::UNIQUE_VIOLATION,
                                    "identity",
                                    "accounts",
                                    "accounts_mobile_hash_key",
                                ) =>
                            {
                                registration::attempt::Status::MobileTaken
                            }
                            Error::Account(account::Error::Database(query))
                                if query.is_constraint(
                                    &SqlState::UNIQUE_VIOLATION,
                                    "identity",
                                    "accounts",
                                    "accounts_id_number_key",
                                ) =>
                            {
                                registration::attempt::Status::IdNumberTaken
                            }
                            _ => return Err(error),
                        };
                        if let Err(rollback) = client
                            .batch_execute(
                                "ROLLBACK TO SAVEPOINT creation; RELEASE SAVEPOINT creation",
                            )
                            .await
                        {
                            return Err(Error::Rollback {
                                cause: Box::new(error),
                                rollback: transaction::EndError::Rollback(rollback.into()),
                            });
                        }
                        if let Some(creation) = creation.take() {
                            creation.pending.rolled_back()?;
                        }
                        status
                    }
                }
            }
        };
        database::insert_attempt(
            client,
            input,
            admission,
            &self.keys.encryption,
            &self.keys.mobile,
            &self.keys.input,
            status,
        )
        .await?;
        match status {
            registration::attempt::Status::MobileTaken => Ok(Outcome::MobileTaken),
            registration::attempt::Status::IdNumberTaken => Ok(Outcome::IdNumberTaken),
            registration::attempt::Status::Registered => Err(Error::Facts {
                rule: "registered result requires complete account creation",
            }),
        }
    }

    async fn create_account(
        &self,
        client: &Client,
        input: &Input,
        admission: &Admission<'_>,
        identity: account::Creation,
        creation: &mut Option<Creation>,
    ) -> Result<Outcome, Error> {
        let role =
            role::fetch_default_institution(client, &self.roles, admission.accepted_at).await?;
        let change = holder::first(
            &identity,
            &role,
            holder::Assignment {
                id: appointment::Id::allocate(),
                previous_id: None,
                change_id: role.change_id(),
                operator_id: identity.revision_id,
                revision_id: identity.revision_id,
                source: holder::Source::Registration,
            },
            input.operation_id,
            admission.accepted_at,
        )?;
        creation
            .as_mut()
            .ok_or(Error::Facts {
                rule: "holder creation retains the account's pending commit",
            })?
            .pending
            .extend(
                change
                    .write(client, self.app.keys(), self.app.anchors())
                    .await?,
            )?;
        database::insert_attempt(
            client,
            input,
            admission,
            &self.keys.encryption,
            &self.keys.mobile,
            &self.keys.input,
            registration::attempt::Status::Registered,
        )
        .await?;
        database::insert_receipt(client, input, admission, &identity).await?;
        let (id, token, expires_at, pending) = session::create(
            client,
            &self.app,
            &self.keys.credential,
            &self.settings,
            session::Input {
                source: session::Source::Registration {
                    authentication_id: admission.credential.source.authentication_id,
                    receipt_id: admission.id,
                },
                origin: admission.credential.source.origin.clone(),
                account_id: identity.account_id,
                operation_id: input.operation_id,
                created_at: admission.accepted_at,
            },
        )
        .await?;
        creation
            .as_mut()
            .ok_or(Error::Facts {
                rule: "session issuance retains the account's pending commit",
            })?
            .pending
            .extend(pending)?;
        database::consume(client, input.credential_id).await?;
        Ok(Outcome::Session {
            id,
            token,
            expires_at,
            created: true,
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) mod tests {
    use std::{sync::Arc, time::Duration};

    use chrono::TimeDelta;
    use platform::cryptography::hmac;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use crate::{
        identity::{
            authentication::{
                Database, Error, Supplier, keys, server,
                session::{
                    Receipt,
                    tests::{CODE, account, age, count, input, message},
                },
            },
            mobile::envelope::Envelope,
        },
        registration::Input,
    };
    pub(super) async fn registration_services(
        connections: platform::database::Connections,
        supplier: &Supplier,
        capacity: usize,
    ) -> (crate::runtime::Context, crate::startup::Services) {
        let context = crate::runtime::Context::new(connections)
            .await
            .expect("runtime");
        let services = crate::startup::Services::start(
            crate::database::Barrier::begin(context.clone())
                .await
                .expect("barrier"),
            crate::identity::authentication::inventory(keys()),
            crate::startup::Settings::new(
                capacity,
                crate::identity::message::Limits::default(),
                crate::identity::authentication::session_settings(),
                crate::registration::Limits::default(),
                crate::identity::authentication::avatar_settings(),
            )
            .expect("settings"),
            supplier.client(),
            crate::identity::authentication::unused_client().await,
        )
        .await
        .expect("verified use cases");
        (context, services)
    }

    pub(super) fn registration_input(
        operation: crate::identity::operation::Id,
        credential: &crate::identity::authentication::session::Registration,
        number: &str,
    ) -> crate::registration::Input {
        crate::registration::Input::new(
            operation,
            credential.id(),
            credential.token().text().parse().expect("original token"),
            "Fixture Operator".parse().expect("name"),
            number.parse().expect("number"),
        )
    }

    #[test]
    fn first_original_read_survives_cleanup_without_another_recovery() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::proxied;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for created in [false, true] {
                timeout(Duration::from_secs(20), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = registration_services(proxied(&database, &proxy), &supplier, 4).await;
                    let message_id = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                    let Receipt::Registration(credential) = services.authentication().create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE)).await.expect("R1 credential") else { panic!("unregistered mobile") };
                    let operation = Uuid::new_v4().to_string().parse().expect("operation");
                    let number = if created { "00000020000229007X" } else { "000000200002290010" };
                    let original = services.registration().register_input_for_test(registration_input(operation, &credential, number)).await;
                    if !created { assert!(matches!(original, Err(super::Error::IdNumberTaken))); }
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let sql = "SELECT attempt.id, attempt.status, attempt.accepted_at, (SELECT count(*) FROM identity.receipts WHERE operation_id=$1) AS receipts, (SELECT count(*) FROM identity.registrations WHERE id=$2) AS credentials, (SELECT count(*) FROM identity.sessions session JOIN identity.receipts receipt ON session.receipt_id=receipt.id WHERE receipt.operation_id=$1) AS sessions, (SELECT count(*) FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id JOIN identity.receipts receipt ON session.receipt_id=receipt.id WHERE receipt.operation_id=$1) AS handoffs FROM identity.attempts attempt WHERE attempt.operation_id=$1";
                    let before = observer.client().query_one(sql, &[&operation, &credential.id()]).await.expect("original facts before faulting replay");
                    assert_eq!(before.get::<_,i64>("receipts"), i64::from(created));
                    assert_eq!(before.get::<_,i64>("credentials"), i64::from(!created));
                    assert_eq!(before.get::<_,i64>("sessions"), i64::from(created));
                    assert_eq!(before.get::<_,i64>("handoffs"), i64::from(created));
                    proxy.arm(Fault::StalledReadCommit);
                    let service = services.registration().clone();
                    let command = registration_input(operation, &credential, number);
                    let task = tokio::spawn(async move { service.register_input_for_test(command).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("first original result read and COMMIT completed");
                    proxy.arm(Fault::RejectedBegin);
                    proxy.release();
                    let result = timeout(Duration::from_secs(3), task).await.expect("bounded original handoff").expect("task");
                    let after = observer.client().query_one(sql, &[&operation, &credential.id()]).await.expect("independent facts before any later attempt");
                    assert_eq!(before.get::<_,Uuid>("id"), after.get::<_,Uuid>("id"));
                    assert_eq!(before.get::<_,String>("status"), after.get::<_,String>("status"));
                    assert_eq!(before.get::<_,chrono::DateTime<chrono::Utc>>("accepted_at"), after.get::<_,chrono::DateTime<chrono::Utc>>("accepted_at"));
                    for column in ["receipts", "credentials", "sessions", "handoffs"] { assert_eq!(before.get::<_,i64>(column), after.get::<_,i64>(column)); }
                    if created {
                        let original = original.expect("first successful creation");
                        let replay = result.unwrap_or_else(|error| panic!("known original session: {error:?}"));
                        assert!(original.created()); assert!(!replay.created());
                        assert_eq!(replay.id(), original.id());
                        assert_eq!(replay.token().text(), original.token().text());
                        assert_eq!(replay.expires_at(), original.expires_at());
                    } else { assert!(matches!(result, Err(super::Error::IdNumberTaken)), "known refusal: {:?}", result.err()); }
                    observer.terminate().await.expect("observer ended");
                    services.drain().await.expect("work drained"); context.stop().await.expect("runtime stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no supplier request through replay and drain");
                    supplier.stop().await; proxy.stop().await;
                }).await.expect("original result scenario budget");
            }
        });
    }

    pub(crate) async fn age_facts(
        database: &Database,
        services: &crate::startup::Services,
        elapsed: TimeDelta,
    ) {
        age(database, services.authentication(), elapsed).await;
    }

    #[test]
    fn r1_replacement_and_r2_creation_follow_the_same_mobile_lock_order() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for replacement_first in [true,false] {
                timeout(Duration::from_secs(25),async {
                    let database=Database::new(Arc::clone(&server)).await;
                    account(&database,"13900000000",false).await;
                    let mut supplier=Supplier::new().await;
                    let (context,services)=registration_services(database.connections(&database.user),&supplier,4).await;
                    let first_message=message(&database,services.authentication(),&mut supplier,"13800000000").await;
                    let credential=match services.authentication().create_session(input(Uuid::new_v4(),first_message,"13800000000",CODE)).await.expect("R1") { Receipt::Registration(value)=>value,Receipt::Session(_)=>panic!("unregistered") };
                    age_facts(&database, &services,TimeDelta::seconds(61)).await;
                    let next_message=message(&database,services.authentication(),&mut supplier,"13800000000").await;
                    let blocker=database.connections(&database.user).recover().await.expect("mobile blocker");
                    blocker.client().batch_execute("BEGIN").await.expect("blocker transaction");
                    crate::database::transaction::object(blocker.client(),b"identity/mobile",b"13800000000").await.expect("hold common mobile");
                    let pid:i32=blocker.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("pid").get(0);
                    let observer=database.connections(&database.user).recover().await.expect("observer");
                    let registration=services.registration().clone(); let authentication=services.authentication().clone();
                    let command=registration_input(Uuid::new_v4().to_string().parse().expect("R2 operation"),&credential,"00000020000229007X");
                    let r1=async move { authentication.create_session(input(Uuid::new_v4(),next_message,"13800000000",CODE)).await };
                    let r2=async move { registration.register_input_for_test(command).await };
                    let first=if replacement_first { tokio::spawn(async move { r1.await.expect("first R1 replacement succeeds"); }) } else { tokio::spawn(async move { r2.await.expect("first R2 creation succeeds"); }) };
                    timeout(Duration::from_secs(5),async { loop {
                        if observer.client().query_one("SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE $1=ANY(pg_catalog.pg_blocking_pids(pid))",&[&pid]).await.expect("first caller queued").get::<_,i64>(0)==1 { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("first real mobile waiter");
                    let authentication=services.authentication().clone(); let registration=services.registration().clone();
                    let command=registration_input(Uuid::new_v4().to_string().parse().expect("other R2 operation"),&credential,"00000020000229007X");
                    let second=if replacement_first { tokio::spawn(async move { assert!(matches!(registration.register_input_for_test(command).await,Err(super::Error::Verification))); }) } else { tokio::spawn(async move { authentication.create_session(input(Uuid::new_v4(),next_message,"13800000000",CODE)).await.expect("R1 discovers the new account and retries its complete lock set"); }) };
                    timeout(Duration::from_secs(5),async { loop {
                        if observer.client().query_one("SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE $1=ANY(pg_catalog.pg_blocking_pids(pid))",&[&pid]).await.expect("both family callers queued").get::<_,i64>(0)==2 { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("two real mobile waiters");
                    blocker.client().batch_execute("COMMIT").await.expect("release ordered competitors");
                    first.await.expect("first worker"); second.await.expect("second worker");
                    assert_eq!(count(&database,"SELECT count(*) FROM identity.attempts").await,if replacement_first{0}else{1});
                    assert_eq!(count(&database,"SELECT count(*) FROM identity.registrations").await,if replacement_first{1}else{0});
                    assert_eq!(count(&database,"SELECT count(*) FROM identity.sessions").await,if replacement_first{0}else{2});
                    observer.terminate().await.expect("observer ended");blocker.terminate().await.expect("blocker ended");services.drain().await.expect("drained");context.stop().await.expect("stopped");supplier.stop().await;
                }).await.expect("R1 R2 mobile interleaving budget");
            }
        });
    }

    #[test]
    fn original_completion_uses_first_identity_and_cannot_be_reissued_until_its_last_evidence_ends()
    {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database=Database::new(server).await;
                account(&database,"13900000000",false).await;
                let mut supplier=Supplier::new().await;
                let (context,services)=registration_services(database.connections(&database.user),&supplier,4).await;
                let owner=database.connections("postgres").recover().await.expect("isolated later-state owner");
                let operation=owner.client().query_one("SELECT operation_id FROM access.appointments WHERE source='initialization'",&[]).await.expect("same UUID in separate initialization family").get(0);
                let message_id=message(&database,services.authentication(),&mut supplier,"13800000000").await;
                let credential=match services.authentication().create_session(input(Uuid::new_v4(),message_id,"13800000000",CODE)).await.expect("R1") { Receipt::Registration(value)=>value,Receipt::Session(_)=>panic!("unregistered") };
                let original=services.registration().register_input_for_test(registration_input(operation,&credential,"00000020000229007X")).await.expect("initial R2");
                let row=owner.client().query_one("SELECT receipt.account_id,receipt.person_id,receipt.revision_id,handoff.token,handoff.expires_at FROM identity.receipts receipt JOIN identity.sessions session ON session.receipt_id=receipt.id JOIN identity.handoffs handoff ON handoff.id=session.id WHERE receipt.operation_id=$1",&[&operation]).await.expect("original immutable relationships");
                let account_id:crate::identity::account::Id=row.get("account_id");
                let person_id:crate::identity::person::Id=row.get("person_id");
                let original_revision:crate::identity::person::revision::Id=row.get("revision_id");
                let sealed:Vec<u8>=row.get("token"); let expires_at:chrono::DateTime<chrono::Utc>=row.get("expires_at");
                let manager=Box::pin(crate::access::role::tests::actor(&database,&services,&mut supplier)).await;
                let candidate=services.correction().candidate_by_person(&manager,person_id).await.expect("current correction target");
                let corrected=Box::pin(services.correction().correct(manager.clone(),Uuid::new_v4().to_string().parse().expect("correction operation"),serde_json::from_value(serde_json::json!({"target":candidate.target(),"name":"Corrected Operator","idNumber":"00000020000229007X"})).expect("typed correction"))).await.expect("real correction preserves R2 original revision");
                let revision_id=corrected.correction().id().revision_id();
                let system_input = |selection| serde_json::from_value(serde_json::json!({
                    "operator": {"mobile": "13900000000"},
                    "accountId": account_id,
                    "revisionId": revision_id,
                    "selection": selection
                })).expect("system appointment input");
                let registration = &services.registration().0;
                crate::access::appoint::run(
                    registration.app.clone(), Arc::clone(&registration.keys),
                    Uuid::new_v4().to_string().parse().expect("system operation"),
                    system_input(serde_json::json!({"action": "grant"})), false,
                ).await.expect("later system administrator assignment");
                let replay=services.registration().register_input_for_test(registration_input(operation,&credential,"00000020000229007X")).await.expect("original first identity remains the recovery proof");
                assert_eq!(replay.id(),original.id()); assert_eq!(replay.token().text(),original.token().text()); assert_eq!(replay.expires_at(),original.expires_at());
                let mut changed=registration_input(operation,&credential,"00000020000229007X");changed.name="Corrected Operator".parse().expect("current name");
                assert!(matches!(services.registration().register_input_for_test(changed).await,Err(super::Error::Mismatch)));
                assert_eq!(owner.client().query_one("SELECT revision_id FROM identity.receipts WHERE operation_id=$1",&[&operation]).await.expect("receipt retains original revision").get::<_,crate::identity::person::revision::Id>(0),original_revision);
                let actor=services.current().authenticate(crate::identity::current::Proof::Cookie {id:original.id(),token:original.token().text().parse().expect("original session token")}).await.expect("current registration session");
                services.revocation().revoke(actor,Uuid::new_v4().to_string().parse().expect("logout operation"),crate::identity::revocation::Scope::Current).await.expect("actual later revocation");
                assert!(matches!(services.registration().register_input_for_test(registration_input(operation,&credential,"00000020000229007X")).await,Err(super::Error::Verification)));
                services.registration().retention().run().await.expect("revoked handoff cleanup");
                let runtime=database.connections(&database.user).recover().await.expect("runtime reconstruction challenge");
                let error=runtime.client().execute("INSERT INTO identity.handoffs (id,token,expires_at) VALUES ($1,$2,$3)",&[&original.id(),&sealed,&expires_at]).await.expect_err("old Session cannot recreate its credential");
                assert_eq!(error.code(),Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                runtime.terminate().await.expect("runtime ended");
                let default_role: crate::access::role::Id = owner.client().query_one(
                    "SELECT id FROM access.roles WHERE scope='institution' AND level=1", &[],
                ).await.expect("default role").get(0);
                crate::access::appoint::run(
                    registration.app.clone(), Arc::clone(&registration.keys),
                    Uuid::new_v4().to_string().parse().expect("system revoke operation"),
                    system_input(serde_json::json!({"action": "revoke", "roleId": default_role})), false,
                ).await.expect("replace administrator before manager cancellation");
                services.cancellation().cancel_account(
                    manager, Uuid::new_v4().to_string().parse().expect("cancellation operation"),
                    account_id,
                    serde_json::from_value(serde_json::json!({
                        "personId": person_id, "revisionId": revision_id, "mobile": "13800000000"
                    })).expect("cancellation input"),
                ).await.expect("actual cancellation retains a Holder tombstone");
                assert!(matches!(services.registration().register_input_for_test(registration_input(operation,&credential,"00000020000229007X")).await,Err(super::Error::Verification)));
                assert_eq!(count(&database,"SELECT count(*) FROM identity.attempts").await,1);
                assert!(owner.client().query_one(
                    "SELECT NOT EXISTS(SELECT 1 FROM identity.receipts WHERE operation_id=$1)
                        AND NOT EXISTS(SELECT 1 FROM identity.registrations WHERE id=$2)
                        AND NOT EXISTS(SELECT 1 FROM identity.sessions session JOIN identity.attempts attempt
                            ON attempt.authentication_id=session.authentication_id WHERE attempt.operation_id=$1)",
                    &[&operation, &credential.id()],
                ).await.expect("the original protocol no longer owns current access").get::<_, bool>(0));
                // Age only the detached protocol chain; permanent account and Holder facts keep their times.
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated expired original protocol");
                for sql in [
                    "UPDATE identity.challenges SET created_at=created_at-interval '25 hours',expires_at=expires_at-interval '25 hours',solved_at=solved_at-interval '25 hours',finished_at=finished_at-interval '25 hours' WHERE id=(SELECT challenge_id FROM identity.messages WHERE id=$1)",
                    "UPDATE identity.messages SET created_at=created_at-interval '25 hours',expires_at=expires_at-interval '25 hours',finished_at=finished_at-interval '25 hours' WHERE id=$1",
                    "UPDATE identity.claims SET created_at=created_at-interval '25 hours' WHERE id=$1",
                    "UPDATE identity.checks SET expires_at=expires_at-interval '25 hours' WHERE id=$1",
                    "UPDATE identity.authentications SET finished_at=finished_at-interval '25 hours' WHERE message_id=$1",
                    "UPDATE identity.attempts SET accepted_at=accepted_at-interval '25 hours' WHERE authentication_id IN(SELECT id FROM identity.authentications WHERE message_id=$1)",
                ] {
                    owner.client().execute(sql, &[&message_id]).await.expect("age the original protocol and its retained references");
                }
                owner.client().batch_execute("COMMIT").await.expect("expired protocol fixture committed");
                services.registration().retention().run().await.expect("last successful attempt expired");
                assert_eq!(count(&database,"SELECT count(*) FROM identity.attempts").await,0);
                let new_message=message(&database,services.authentication(),&mut supplier,"13800000000").await;
                let new_credential=match services.authentication().create_session(input(Uuid::new_v4(),new_message,"13800000000",CODE)).await.expect("fresh verification") { Receipt::Registration(value)=>value,Receipt::Session(_)=>panic!("released mobile") };
                let renewed=services.registration().register_input_for_test(registration_input(operation,&new_credential,"00000020000229007X")).await.expect("the cleaned operation can qualify again");
                assert_ne!(renewed.id(),original.id());
                assert_eq!(count(&database,"SELECT count(*) FROM identity.persons").await,3);
                assert_eq!(owner.client().query_one("SELECT count(DISTINCT person_id) FROM access.appointments WHERE operation_id=$1",&[&operation]).await.expect("initialization and both registrations retain independent histories").get::<_,i64>(0),3);
                owner.terminate().await.expect("fixture ended");services.drain().await.expect("drained");context.stop().await.expect("stopped");
                let (context, services) = Box::pin(registration_services(database.connections(&database.user), &supplier, 4)).await;
                services.drain().await.expect("both permanent creation histories survive restart"); context.stop().await.expect("restarted runtime stopped"); supplier.stop().await;
            })).await.expect("original completion lifecycle budget");
        }));
    }

    #[test]
    fn configured_lower_hourly_quota_counts_committed_refusals_and_preserves_original_results() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                assert_eq!(super::Limits::default().hourly(), 5);
                assert!(super::Limits::new(0).is_err());
                assert!(super::Limits::new(6).is_err());
                let database = Database::new(server).await;
                account(&database, "13900000000", false).await;
                let mut supplier = Supplier::new().await;
                let context = crate::runtime::Context::new(database.connections(&database.user))
                    .await
                    .expect("runtime");
                let services = crate::startup::Services::start(
                    crate::database::Barrier::begin(context.clone())
                        .await
                        .expect("barrier"),
                    crate::identity::authentication::inventory(keys()),
                    crate::startup::Settings::new(
                        4,
                        crate::identity::message::Limits::default(),
                        crate::identity::authentication::session_settings(),
                        super::Limits::new(2).expect("lower configured quota"),
                        crate::identity::authentication::avatar_settings(),
                    )
                    .expect("settings"),
                    supplier.client(),
                    crate::identity::authentication::unused_client().await,
                )
                .await
                .expect("startup");
                let message_id = message(
                    &database,
                    services.authentication(),
                    &mut supplier,
                    "13800000000",
                )
                .await;
                let credential = match services
                    .authentication()
                    .create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE))
                    .await
                    .expect("R1")
                {
                    Receipt::Registration(value) => value,
                    Receipt::Session(_) => panic!("unregistered"),
                };
                let operation = Uuid::new_v4().to_string().parse().expect("operation");
                for operation in [
                    operation,
                    Uuid::new_v4()
                        .to_string()
                        .parse()
                        .expect("second operation"),
                ] {
                    assert!(matches!(
                        services
                            .registration()
                            .register_input_for_test(registration_input(
                                operation,
                                &credential,
                                "000000200002290010"
                            ))
                            .await,
                        Err(super::Error::IdNumberTaken)
                    ));
                }
                assert_eq!(
                    count(&database, "SELECT count(*) FROM identity.attempts").await,
                    2
                );
                assert!(matches!(
                    services
                        .registration()
                        .register_input_for_test(registration_input(
                            Uuid::new_v4().to_string().parse().expect("third operation"),
                            &credential,
                            "00000020000229007X"
                        ))
                        .await,
                    Err(super::Error::Quota { .. })
                ));
                assert!(matches!(
                    services
                        .registration()
                        .register_input_for_test(registration_input(
                            operation,
                            &credential,
                            "000000200002290010"
                        ))
                        .await,
                    Err(super::Error::IdNumberTaken)
                ));
                assert_eq!(
                    count(&database, "SELECT count(*) FROM identity.attempts").await,
                    2
                );
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                supplier.stop().await;
            })
            .await
            .expect("configured registration quota budget");
        });
    }

    #[test]
    fn account_unique_conflicts_rollback_only_creation_before_committing_the_refusal() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for occupied in ["mobile", "number"] {
                timeout(Duration::from_secs(25), async {
                    let database = Database::new(Arc::clone(&server)).await;
                    let existing = account(&database, "13900000000", false).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = registration_services(database.connections(&database.user), &supplier, 4).await;
                    let message_id = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                    let credential = match services.authentication().create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE)).await.expect("R1") { Receipt::Registration(value) => value, Receipt::Session(_) => panic!("unregistered") };
                    let operation = Uuid::new_v4().to_string().parse().expect("operation");
                    let owner = database.connections("postgres").recover().await.expect("isolated concurrent state fixture");
                    owner.client().batch_execute("CREATE FUNCTION identity.pause_creation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(820114::bigint); RETURN NEW; END $$; CREATE TRIGGER pause_creation BEFORE INSERT ON identity.accounts FOR EACH ROW EXECUTE FUNCTION identity.pause_creation(); BEGIN; SELECT pg_advisory_xact_lock(820114::bigint)").await.expect("pause after the occupation read and before the unique index");
                    let pid: i32 = owner.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("blocker pid").get(0);
                    let worker = services.registration().clone();
                    let command = registration_input(operation, &credential, "00000020000229007X");
                    let task = tokio::spawn(async move { worker.register_input_for_test(command).await });
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    timeout(Duration::from_secs(5), async { loop {
                        if observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE $1=ANY(pg_catalog.pg_blocking_pids(pid)))", &[&pid]).await.expect("registration reached actual account insert").get::<_,bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("unique conflict barrier");
                    owner.client().batch_execute("SET LOCAL session_replication_role=replica").await.expect("isolated future state injection");
                    let material = keys();
                    if occupied == "mobile" {
                        let value = Envelope::seal(&material.encryption, &material.mobile, crate::identity::mobile::envelope::Binding::Account(existing), &"13800000000".parse().expect("new mobile")).expect("existing account binding");
                        owner.client().execute("UPDATE identity.accounts SET mobile=$1 WHERE id=$2", &[&value, &existing]).await.expect("concurrent mobile ownership");
                    } else {
                        let number = "00000020000229007X".parse().expect("new number");
                        let revision_id = owner.client().query_one("SELECT person.revision_id FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id WHERE account.id=$1", &[&existing]).await.expect("existing revision").get(0);
                        let history = services.registration().0.app.keys();
                        let profile = crate::identity::person::revision::fetch(owner.client(), history, revision_id).await.expect("original version");
                        crate::identity::person::revision::fixture_update(owner.client(), history, revision_id, profile.person_id(), profile.name(), &number).await.expect("consistent sealed identity number");
                        owner.client().execute("UPDATE identity.accounts SET id_number=$1 WHERE id=$2", &[&material.number.hash(&number).expect("number hash"), &existing]).await.expect("concurrent number ownership");
                    }
                    owner.client().batch_execute("COMMIT").await.expect("conflict becomes visible before insert resumes");
                    let result = task.await.expect("registration work");
                    if occupied == "mobile" { assert!(matches!(result, Err(super::Error::MobileTaken))); } else { assert!(matches!(result, Err(super::Error::IdNumberTaken))); }
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM identity.persons) AS persons, (SELECT count(*) FROM identity.revisions) AS revisions, (SELECT count(*) FROM identity.accounts) AS accounts, (SELECT count(*) FROM identity.avatars) AS avatars, (SELECT count(*) FROM access.appointments) AS appointments, (SELECT count(*) FROM identity.receipts) AS receipts, (SELECT count(*) FROM identity.sessions) AS sessions, (SELECT count(*) FROM identity.registrations WHERE id=$1) AS credentials, (SELECT status FROM identity.attempts WHERE operation_id=$2) AS status", &[&credential.id(), &operation]).await.expect("independent savepoint effects before replay");
                    for field in ["persons", "revisions", "accounts", "avatars", "appointments", "credentials"] { assert_eq!(row.get::<_,i64>(field),1,"{occupied}: {field}"); }
                    assert_eq!(row.get::<_,i64>("receipts"),0); assert_eq!(row.get::<_,i64>("sessions"),0);
                    assert_eq!(row.get::<_,&str>("status"), if occupied=="mobile" { "mobile_taken" } else { "id_number_taken" });
                    let replay = services.registration().register_input_for_test(registration_input(operation, &credential, "00000020000229007X")).await;
                    if occupied == "mobile" { assert!(matches!(replay, Err(super::Error::MobileTaken))); } else { assert!(matches!(replay, Err(super::Error::IdNumberTaken))); }
                    services.ready().await.expect("confirmed savepoint rollback settles the new Person candidate and preserves admission");
                    owner.client().batch_execute("DROP TRIGGER pause_creation ON identity.accounts; DROP FUNCTION identity.pause_creation()").await.expect("remove isolated gate");
                    let message_id = message(&database, services.authentication(), &mut supplier, "13700000000").await;
                    let credential = match services.authentication().create_session(input(Uuid::new_v4(), message_id, "13700000000", CODE)).await.expect("independent R1 after the refused creation") {
                        Receipt::Registration(value) => value,
                        Receipt::Session(_) => panic!("new mobile"),
                    };
                    services.registration().register_input_for_test(registration_input(
                        Uuid::new_v4().to_string().parse().expect("new operation"), &credential, "000000200002290029",
                    )).await.expect("an independent creation succeeds after the settled refusal");
                    owner.terminate().await.expect("fixture ended"); observer.terminate().await.expect("observer ended"); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
                }).await.expect("exact unique constraint budget");
            }
        });
    }

    #[test]
    fn complete_startup_rejects_registration_corruption_and_still_checks_sessions_without_handoffs()
    {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for corruption in [
                "UPDATE identity.attempts SET status='id_number_taken'",
                "UPDATE identity.attempts SET mobile.value=set_byte((mobile).value,octet_length((mobile).value)-1,get_byte((mobile).value,octet_length((mobile).value)-1)#1)",
                "UPDATE identity.receipts SET revision_id=(SELECT revision_id FROM identity.persons WHERE id<>identity.receipts.person_id LIMIT 1)",
                "UPDATE identity.sessions SET app_id='different-website' WHERE receipt_id IS NOT NULL",
            ] {
                timeout(Duration::from_secs(25), async {
                    let database = Database::new(Arc::clone(&server)).await;
                    account(&database, "13900000000", false).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = registration_services(database.connections(&database.user), &supplier, 4).await;
                    let message_id = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                    let credential = match services.authentication().create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE)).await.expect("R1") { Receipt::Registration(value) => value, Receipt::Session(_) => panic!("unregistered") };
                    services.registration().register_input_for_test(registration_input(Uuid::new_v4().to_string().parse().expect("operation"), &credential, "00000020000229007X")).await.expect("R2");
                    age_facts(&database, &services, TimeDelta::minutes(16)).await;
                    services.registration().retention().run().await.expect("expired handoff cleanup");
                    assert_eq!(count(&database,"SELECT count(*) FROM identity.handoffs").await,0);
                    assert_eq!(count(&database,"SELECT count(*) FROM identity.sessions").await,1);
                    services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    let (context, services) = registration_services(database.connections(&database.user), &supplier, 4).await;
                    services.drain().await.expect("healthy no-handoff startup"); context.stop().await.expect("healthy stopped");
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("corruption transaction");
                    owner.client().batch_execute(corruption).await.expect("targeted corrupt history");
                    owner.client().batch_execute("COMMIT").await.expect("corruption committed"); owner.terminate().await.expect("owner ended");
                    let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart runtime");
                    let result = crate::startup::Services::start(crate::database::Barrier::begin(context.clone()).await.expect("restart barrier"), crate::identity::authentication::inventory(keys()), crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"), supplier.client(), crate::identity::authentication::unused_client().await).await;
                    let error = result.err().expect("corruption rejects startup");
                    if corruption.starts_with("UPDATE identity.sessions") {
                        assert!(matches!(error, crate::startup::Error::Identity(
                            crate::identity::verification::Error::History(
                                crate::history::Error::Seal(crate::keys::seal::Error::Mismatch)
                            )
                        )), "{corruption}: {error:?}");
                    } else {
                        assert!(matches!(error, crate::startup::Error::Registration(_)), "{corruption}: {error:?}");
                    }
                    context.stop().await.expect("failed startup driver stopped"); supplier.stop().await;
                }).await.expect("registration startup corruption budget");
            }
        });
    }

    #[test]
    fn runtime_cannot_commit_partial_creation_privileged_assignments_or_forged_completion() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                let database = Database::new(server).await;
                account(&database, "13900000000", false).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    registration_services(database.connections(&database.user), &supplier, 4).await;
                let message_id = message(
                    &database,
                    services.authentication(),
                    &mut supplier,
                    "13800000000",
                )
                .await;
                let credential = match services
                    .authentication()
                    .create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE))
                    .await
                    .expect("R1")
                {
                    Receipt::Registration(value) => value,
                    Receipt::Session(_) => panic!("unregistered"),
                };
                let command = registration_input(
                    Uuid::new_v4().to_string().parse().expect("operation"),
                    &credential,
                    "00000020000229007X",
                );
                let runtime = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("runtime SQL");
                let material = keys();
                let source = super::database::find_credential(runtime.client(), credential.id())
                    .await
                    .expect("source query")
                    .expect("credential");
                let mobile = "13800000000".parse().expect("mobile");
                for defect in [
                    "identity_only",
                    "missing_receipt",
                    "missing_session",
                    "unconsumed_credential",
                    "session_application",
                ] {
                    let now = context.sample().await.expect("trusted time");
                    let admission = super::Admission {
                        id: crate::identity::registration::attempt::Id::allocate(),
                        credential: &source,
                        mobile: &mobile,
                        accepted_at: now,
                    };
                    let identity = crate::identity::account::Creation::allocate();
                    let mut pending = None;
                    runtime
                        .client()
                        .batch_execute("BEGIN")
                        .await
                        .expect("runtime transaction");
                    identity
                        .prepare(
                            crate::identity::account::Input {
                                mobile: &mobile,
                                name: &command.name,
                                id_number: &command.id_number,
                            },
                            crate::identity::account::Keys {
                                mobile: &material.mobile,
                                encryption: &material.encryption,
                                number: &material.number,
                            },
                            crate::identity::account::record::Source::Registration,
                            command.operation_id,
                            now,
                        )
                        .expect("sealed account construction")
                        .write_offline(runtime.client(), services.registration().0.app.keys())
                        .await
                        .expect("complete identity rows before deferred checks");
                    if defect != "identity_only" {
                        let role = crate::access::role::fetch_default_institution(
                            runtime.client(),
                            &services.registration().0.roles,
                            now,
                        )
                        .await
                        .expect("ordinary role");
                        crate::access::holder::first(
                            &identity,
                            &role,
                            crate::access::holder::Assignment {
                                id: crate::access::appointment::Id::allocate(),
                                previous_id: None,
                                change_id: role.change_id(),
                                operator_id: identity.revision_id,
                                revision_id: identity.revision_id,
                                source: crate::access::holder::Source::Registration,
                            },
                            command.operation_id,
                            now,
                        )
                        .expect("valid first assignment")
                        .write_offline(runtime.client(), services.registration().0.app.keys())
                        .await
                        .expect("paired first assignment");
                        super::database::insert_attempt(
                            runtime.client(),
                            &command,
                            &admission,
                            &material.encryption,
                            &material.mobile,
                            &material.input,
                            crate::identity::registration::attempt::Status::Registered,
                        )
                        .await
                        .expect("attempt insert");
                        if defect != "missing_receipt" {
                            super::database::insert_receipt(
                                runtime.client(),
                                &command,
                                &admission,
                                &identity,
                            )
                            .await
                            .expect("receipt insert");
                            if defect != "missing_session" {
                                let settings = if defect == "session_application" {
                                    crate::identity::session::settings::Settings::new(
                                        "other-website".parse().expect("different application"),
                                        3600,
                                        600,
                                    )
                                    .expect("valid lifetimes")
                                } else {
                                    services.registration().0.settings.clone()
                                };
                                let (_, _, _, candidate) = crate::identity::session::create(
                                    runtime.client(),
                                    &services.registration().0.app,
                                    &material.credential,
                                    &settings,
                                    crate::identity::session::Input {
                                        source: crate::identity::session::Source::Registration {
                                            authentication_id: source.source.authentication_id,
                                            receipt_id: admission.id,
                                        },
                                        origin: crate::identity::session::Origin::WebsiteMessage(
                                            settings.app_id().clone(),
                                        ),
                                        account_id: identity.account_id,
                                        operation_id: command.operation_id,
                                        created_at: now,
                                    },
                                )
                                .await
                                .expect("session and handoff");
                                pending = Some(candidate);
                            }
                            if defect != "unconsumed_credential" {
                                super::database::consume(runtime.client(), credential.id())
                                    .await
                                    .expect("credential consumption");
                            }
                        }
                    }
                    let error = runtime
                        .client()
                        .batch_execute("COMMIT")
                        .await
                        .expect_err("incomplete or privileged creation cannot commit");
                    assert_eq!(
                        error.code(),
                        Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION),
                        "{defect}: {error}"
                    );
                    if let Some(pending) = pending {
                        pending
                            .rolled_back()
                            .expect("rejected creation settles its Session candidate");
                    }
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.accounts").await,
                        1,
                        "{defect}"
                    );
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.attempts").await,
                        0,
                        "{defect}"
                    );
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.receipts").await,
                        0,
                        "{defect}"
                    );
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.sessions").await,
                        0,
                        "{defect}"
                    );
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.registrations").await,
                        1,
                        "{defect}"
                    );
                }
                services
                    .registration()
                    .register_input_for_test(command)
                    .await
                    .expect("the unchanged credential still completes normally");
                for sql in [
                    "DELETE FROM identity.attempts",
                    "DELETE FROM identity.handoffs",
                    "DELETE FROM identity.sessions",
                ] {
                    let error = runtime
                        .client()
                        .batch_execute(sql)
                        .await
                        .expect_err("fresh recovery facts remain protected");
                    assert_eq!(
                        error.code(),
                        Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)
                    );
                }
                let error = runtime
                    .client()
                    .batch_execute("DELETE FROM identity.receipts")
                    .await
                    .expect_err("receipt removal requires the same account cancellation");
                assert_eq!(
                    error.code(),
                    Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)
                );
                runtime.terminate().await.expect("runtime ended");
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                supplier.stop().await;
            })
            .await
            .expect("runtime creation protection budget");
        });
    }

    #[test]
    fn committed_refusals_count_once_and_survive_replacement_while_invalid_inputs_do_not_count() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                let database = Database::new(server).await;
                account(&database, "13900000000", false).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = registration_services(database.connections(&database.user), &supplier, 4).await;
                let message_id = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                let credential = match services.authentication().create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE)).await.expect("R1 registration") {
                    Receipt::Registration(credential) => credential,
                    Receipt::Session(_) => panic!("unregistered mobile"),
                };
                let operation = Uuid::new_v4().to_string().parse().expect("original operation");
                let wrong = crate::registration::Input::new(operation, credential.id(), "__________________________________________8".parse().expect("wrong valid token"), "Different Name".parse().expect("name"), "000000200002290010".parse().expect("number"));
                assert!(matches!(services.registration().register_input_for_test(wrong).await, Err(super::Error::Proof)));
                assert!(matches!(services.registration().register_input_for_test(registration_input(operation, &credential, "000000210003010016")).await, Err(super::Error::Date(crate::identity::person::number::ParseError::Future))));
                assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 0);
                for index in 0..5 {
                    let operation = if index == 0 { operation } else { Uuid::new_v4().to_string().parse().expect("new operation") };
                    assert!(matches!(services.registration().register_input_for_test(registration_input(operation, &credential, "000000200002290010")).await, Err(super::Error::IdNumberTaken)));
                }
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let row = observer.client().query_one("SELECT count(*) AS admissions, count(DISTINCT operation_id) AS originals, bool_and(status='id_number_taken' AND credential_id=$1) AS refusal FROM identity.attempts", &[&credential.id()]).await.expect("independent quota before replay");
                assert_eq!(row.get::<_,i64>("admissions"),5); assert_eq!(row.get::<_,i64>("originals"),5); assert!(row.get::<_,bool>("refusal"));
                assert!(matches!(services.registration().register_input_for_test(registration_input(Uuid::new_v4().to_string().parse().expect("sixth operation"), &credential, "00000020000229007X")).await, Err(super::Error::Quota { .. })));
                assert!(matches!(services.registration().register_input_for_test(registration_input(operation, &credential, "000000200002290010")).await, Err(super::Error::IdNumberTaken)));
                age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                let next_message = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                let replacement = match services.authentication().create_session(input(Uuid::new_v4(), next_message, "13800000000", CODE)).await.expect("R1 replacement") {
                    Receipt::Registration(credential) => credential,
                    Receipt::Session(_) => panic!("mobile is still unregistered"),
                };
                assert_ne!(credential.id(), replacement.id());
                assert!(matches!(services.registration().register_input_for_test(registration_input(operation, &credential, "000000200002290010")).await, Err(super::Error::IdNumberTaken)));
                assert!(matches!(services.registration().register_input_for_test(registration_input(Uuid::new_v4().to_string().parse().expect("old credential attempt"), &credential, "00000020000229007X")).await, Err(super::Error::Verification)));
                assert!(matches!(services.registration().register_input_for_test(registration_input(Uuid::new_v4().to_string().parse().expect("replacement attempt"), &replacement, "00000020000229007X")).await, Err(super::Error::Quota { .. })));
                assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 5);
                assert_eq!(count(&database, "SELECT count(*) FROM identity.receipts").await, 0);
                observer.terminate().await.expect("observer ended"); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            }).await.expect("quota and replacement budget");
        });
    }

    #[test]
    fn a_mobile_occupied_after_r1_is_a_durable_refusal_even_when_the_account_is_suspended_or_cancelled()
     {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = crate::identity::transfer::tests::start(&database, &supplier, &sms).await;
                let manager = Box::pin(crate::access::role::tests::actor(&database, &services, &mut supplier)).await;
                let target = Box::pin(crate::access::appointment::tests::register(&database, &services, &mut supplier)).await;
                let account = services.current().read(&target).await.expect("current target");
                let previous_attempts = count(&database, "SELECT count(*) FROM identity.attempts").await;
                let message_id = message(&database, services.authentication(), &mut supplier, "13600000000").await;
                let credential = match services.authentication().create_session(input(Uuid::new_v4(), message_id, "13600000000", CODE)).await.expect("original unoccupied mobile") { Receipt::Registration(value) => value, Receipt::Session(_) => panic!("no account yet") };
                age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                let confirmation = serde_json::json!({
                    "personId": account.person_id(),
                    "revisionId": account.revision_id(),
                    "mobile": "13800000000",
                });
                Box::pin(services.suspension().suspend(
                    manager.clone(), Uuid::new_v4().to_string().parse().expect("suspension operation"),
                    account.id(), serde_json::from_value(confirmation.clone()).expect("suspension confirmation"),
                )).await.expect("paused account still occupies its phone");
                let mut verified = confirmation;
                verified["verified"] = serde_json::json!(true);
                let transfer = Box::pin(services.account().create_transfer(
                    manager.clone(), Uuid::new_v4().to_string().parse().expect("transfer operation"),
                    account.id(), serde_json::from_value(verified).expect("verified transfer confirmation"),
                )).await.expect("assisted transfer permits a paused target");
                let (_, message, code) = crate::identity::transfer::tests::bind_to(
                    &database, &services, &manager, transfer.id(), &mut supplier, &mut sms, "13600000000",
                ).await;
                services.transfer().complete_assisted(
                    manager.clone(), account.id(), crate::identity::transfer::completion::Input::new(
                        Uuid::new_v4().to_string().parse().expect("completion operation"),
                        "13600000000".parse().expect("new occupied mobile"), message.id(), code.parse().expect("delivered code"),
                    ),
                ).await.expect("mobile becomes occupied after the original R1");
                let operation = Uuid::new_v4().to_string().parse().expect("operation");
                assert!(matches!(services.registration().register_input_for_test(registration_input(operation, &credential, "000000200002290029")).await, Err(super::Error::MobileTaken)));
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let row = observer.client().query_one("SELECT attempt.status='mobile_taken' AND attempt.credential_id=registration.id AND attempt.authentication_id=registration.authentication_id AND (attempt.mobile).hash=(registration.mobile).hash AND EXISTS (SELECT 1 FROM identity.accounts account JOIN identity.suspensions suspension ON suspension.id=account.suspension_id WHERE account.id=$2 AND (account.mobile).hash=(attempt.mobile).hash) AS paired FROM identity.attempts attempt JOIN identity.registrations registration ON registration.id=attempt.credential_id WHERE attempt.operation_id=$1", &[&operation, &account.id()]).await.expect("independent suspended occupancy refusal");
                assert!(row.get::<_,bool>("paired"));
                let cancellation = services.cancellation().cancel_account(
                    manager, Uuid::new_v4().to_string().parse().expect("cancellation operation"),
                    account.id(), serde_json::from_value(serde_json::json!({
                        "personId": account.person_id(), "revisionId": account.revision_id(), "mobile": "13600000000",
                    })).expect("current cancellation confirmation"),
                ).await.expect("lawful cancellation releases the occupied phone");
                assert!(observer.client().query_one("SELECT cancellation_id=$2 AND mobile IS NULL FROM identity.accounts WHERE id=$1", &[&account.id(), &cancellation.id()]).await.expect("released occupancy and permanent tombstone").get::<_, bool>(0));
                assert!(matches!(services.registration().register_input_for_test(registration_input(operation, &credential, "000000200002290029")).await, Err(super::Error::MobileTaken)));
                assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, previous_attempts + 1);
                assert_eq!(count(&database, "SELECT count(*) FROM identity.registrations").await, 1);
                observer.terminate().await.expect("observer ended"); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); sms.stop().await; supplier.stop().await;
            })).await.expect("late mobile occupation budget");
        }));
    }

    #[test]
    fn competing_credentials_and_identity_numbers_create_only_one_formal_account() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for same_credential in [true, false] {
                timeout(Duration::from_secs(25), async {
                    let database = Database::new(Arc::clone(&server)).await;
                    account(&database, "13900000000", false).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = registration_services(database.connections(&database.user), &supplier, 4).await;
                    let first_message = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                    let first = match services.authentication().create_session(input(Uuid::new_v4(), first_message, "13800000000", CODE)).await.expect("first credential") { Receipt::Registration(value) => value, Receipt::Session(_) => panic!("unregistered") };
                    let second_message = message(&database, services.authentication(), &mut supplier, "13800000001").await;
                    let second = match services.authentication().create_session(input(Uuid::new_v4(), second_message, "13800000001", CODE)).await.expect("second credential") { Receipt::Registration(value) => value, Receipt::Session(_) => panic!("unregistered") };
                    let first_command = registration_input(Uuid::new_v4().to_string().parse().expect("first operation"), &first, "00000020000229007X");
                    let second_command = registration_input(Uuid::new_v4().to_string().parse().expect("second operation"), if same_credential { &first } else { &second }, "00000020000229007X");
                    let blocker = database.connections(&database.user).recover().await.expect("shared number blocker");
                    blocker.client().batch_execute("BEGIN").await.expect("blocker transaction");
                    crate::database::transaction::object(blocker.client(), b"identity/id_number", b"00000020000229007X").await.expect("hold competing number");
                    let pid: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("blocker pid").get(0);
                    let first_service = services.registration().clone(); let second_service = services.registration().clone();
                    let first_task = tokio::spawn(async move { first_service.register_input_for_test(first_command).await });
                    let second_task = tokio::spawn(async move { second_service.register_input_for_test(second_command).await });
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    timeout(Duration::from_secs(5), async {
                        loop {
                            let row = observer.client().query_one("WITH RECURSIVE waiting AS (SELECT pid FROM pg_catalog.pg_stat_activity WHERE $1=ANY(pg_catalog.pg_blocking_pids(pid)) UNION SELECT activity.pid FROM pg_catalog.pg_stat_activity activity JOIN waiting ON waiting.pid=ANY(pg_catalog.pg_blocking_pids(activity.pid))) SELECT (SELECT count(*) FROM waiting) AS total, (SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE $1=ANY(pg_catalog.pg_blocking_pids(pid))) AS direct", &[&pid]).await.expect("both competing backends reach their lock chain");
                            if row.get::<_,i64>("total") == 2 {
                                assert_eq!(row.get::<_,i64>("direct"), if same_credential { 1 } else { 2 });
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("competition reaches number lock");
                    assert!(!first_task.is_finished() && !second_task.is_finished());
                    blocker.client().batch_execute("COMMIT").await.expect("release competition");
                    let first = first_task.await.expect("first worker"); let second = second_task.await.expect("second worker");
                    let refusal = match (first, second) { (Ok(_), Err(error)) | (Err(error), Ok(_)) => error, _ => panic!("exactly one success") };
                    if same_credential { assert!(matches!(refusal, super::Error::Verification)); } else { assert!(matches!(refusal, super::Error::IdNumberTaken)); }
                    assert_eq!(count(&database, "SELECT count(*) FROM identity.receipts").await, 1);
                    assert_eq!(count(&database, "SELECT count(*) FROM identity.accounts").await, 2);
                    assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, if same_credential { 1 } else { 2 });
                    observer.terminate().await.expect("observer ended"); blocker.terminate().await.expect("blocker ended"); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
                }).await.expect("registration competition budget");
            }
        });
    }

    #[test]
    fn registration_cancellation_and_unknown_commit_keep_shared_work_and_original_identity() {
        use std::{future::Future, task::Poll};

        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::proxied;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for unknown in [false, true] {
                timeout(Duration::from_secs(25), async {
                    let database = Database::new(Arc::clone(&server)).await;
                    account(&database, "13900000000", false).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) =
                        registration_services(proxied(&database, &proxy), &supplier, 1).await;
                    let message_id = message(
                        &database,
                        services.authentication(),
                        &mut supplier,
                        "13800000000",
                    )
                    .await;
                    let credential = match services
                        .authentication()
                        .create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE))
                        .await
                        .expect("R1 registration")
                    {
                        Receipt::Registration(credential) => credential,
                        Receipt::Session(_) => panic!("unregistered mobile"),
                    };
                    let operation = Uuid::new_v4().to_string().parse().expect("operation");
                    let command = registration_input(operation, &credential, "00000020000229007X");
                    proxy.arm(if unknown {
                        Fault::UnknownCommit
                    } else {
                        Fault::DelayedCommit
                    });
                    let worker = services.registration().clone();
                    let caller =
                        tokio::spawn(async move { worker.register_input_for_test(command).await });
                    timeout(Duration::from_secs(5), proxy.intercepted())
                        .await
                        .expect("original commit held");
                    if unknown {
                        assert!(matches!(
                            timeout(Duration::from_secs(5), caller)
                                .await
                                .expect("unknown bounded")
                                .expect("worker"),
                            Err(crate::registration::Error::Unknown { .. })
                        ));
                    } else {
                        caller.abort();
                        assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
                        assert!(matches!(
                            services
                                .authentication()
                                .create_session(input(
                                    Uuid::new_v4(),
                                    message_id,
                                    "13800000000",
                                    CODE
                                ))
                                .await,
                            Err(Error::Busy)
                        ));
                    }
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.attempts").await,
                        0
                    );
                    let valid =
                        services
                            .registration()
                            .register_input_for_test(registration_input(
                                operation,
                                &credential,
                                "00000020000229007X",
                            ));
                    let invalid = services.registration().register_input_for_test(
                        crate::registration::Input::new(
                            operation,
                            credential.id(),
                            "__________________________________________8"
                                .parse()
                                .expect("wrong valid token"),
                            "Different Operator".parse().expect("other name"),
                            "000000200002290010".parse().expect("other number"),
                        ),
                    );
                    assert!(matches!(
                        timeout(Duration::from_secs(2), invalid)
                            .await
                            .expect("wrong proof rejected before original commit is released"),
                        Err(crate::registration::Error::Proof)
                    ));
                    if !unknown {
                        let mut changed =
                            registration_input(operation, &credential, "00000020000229007X");
                        changed.name = "Different Operator".parse().expect("other name");
                        assert!(matches!(
                            timeout(
                                Duration::from_secs(2),
                                services.registration().register_input_for_test(changed)
                            )
                            .await
                            .expect("different input rejected before original commit is released"),
                            Err(crate::registration::Error::Mismatch)
                        ));
                    }
                    tokio::pin!(valid);
                    std::future::poll_fn(|context| {
                        assert!(valid.as_mut().poll(context).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                    proxy.release();
                    timeout(Duration::from_secs(5), async {
                        while count(
                            &database,
                            "SELECT count(*) FROM identity.attempts WHERE status='registered'",
                        )
                        .await
                            == 0
                        {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("independently observe late commit before waiting for replay");
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.receipts").await,
                        1
                    );
                    let original = valid.await.expect("original same-proof result");
                    assert!(
                        !original.created(),
                        "a later waiter did not create the recovered session"
                    );
                    services
                        .drain()
                        .await
                        .expect("all original and waiting work ended");
                    context.stop().await.expect("runtime stopped");
                    let (context, services) =
                        registration_services(database.connections(&database.user), &supplier, 2)
                            .await;
                    let replay = services
                        .registration()
                        .register_input_for_test(registration_input(
                            operation,
                            &credential,
                            "00000020000229007X",
                        ))
                        .await
                        .expect("restart original");
                    assert!(!replay.created());
                    assert_eq!(original.id(), replay.id());
                    assert_eq!(original.token().text(), replay.token().text());
                    assert_eq!(original.expires_at(), replay.expires_at());
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.attempts").await,
                        1
                    );
                    services.drain().await.expect("restart drained");
                    context.stop().await.expect("restart stopped");
                    supplier.stop().await;
                    proxy.stop().await;
                })
                .await
                .expect("R2 cancellation and unknown budget");
            }
        });
    }

    #[test]
    fn rejected_requests_reuse_confirmed_transactions_and_discard_failed_rollbacks() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::proxied;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = registration_services(proxied(&database, &proxy), &supplier, 4).await;
                let message_id = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                let credential = match services.authentication().create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE)).await.expect("R1 credential") {
                    Receipt::Registration(credential) => credential,
                    Receipt::Session(_) => panic!("unregistered mobile"),
                };
                let operation = Uuid::new_v4().to_string().parse().expect("original operation");
                assert!(matches!(services.registration().register_input_for_test(registration_input(operation, &credential, "000000200002290010")).await, Err(super::Error::IdNumberTaken)));
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let held = context.connections().lease().await.expect("occupy the other pool slot");
                let lease = context.connections().lease().await.expect("only available pool backend");
                let mut pid: i32 = lease.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("original backend").get(0);
                lease.release();
                for failed in [false, true] {
                    for kind in ["proof", "mismatch", "verification", "date"] {
                        let mut command = registration_input(operation, &credential, "000000200002290010");
                        let expected = match kind {
                            "proof" => { command.token = "__________________________________________8".parse().expect("wrong valid token"); super::Failure::Proof }
                            "mismatch" => { command.name = "Different Operator".parse().expect("other name"); super::Failure::Mismatch }
                            "verification" => { command.operation_id = Uuid::new_v4().to_string().parse().expect("new operation"); command.credential_id = Uuid::new_v4().to_string().parse().expect("absent credential"); super::Failure::Verification }
                            _ => { command.operation_id = Uuid::new_v4().to_string().parse().expect("new operation"); command.id_number = "000000210003010016".parse().expect("future date"); super::Failure::InvalidDate }
                        };
                        if failed { proxy.arm(Fault::RejectedRollback); }
                        let error = services.registration().register_input_for_test(command).await.err().expect("rejected request");
                        if failed {
                            timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual rejection ROLLBACK interrupted");
                            assert_eq!(error.failure(), super::Failure::Unavailable);
                            let error = match error {
                                super::Error::Combined { cause, termination } => {
                                    assert!(matches!(termination, platform::database::Error::Postgres(_)), "{termination:?}");
                                    *cause
                                }
                                error => error,
                            };
                            match error {
                                super::Error::Rollback { cause, rollback: crate::database::transaction::EndError::Rollback(rollback) } => {
                                    assert_eq!(cause.failure(), expected);
                                    assert!(rollback.0.is_closed(), "{rollback:?}");
                                }
                                error => panic!("both original refusal and rollback failure required: {error:?}"),
                            }
                        } else {
                            assert_eq!(error.failure(), expected);
                            let idle: bool = observer.client().query_one("SELECT state='idle' AND backend_xid IS NULL AND backend_xmin IS NULL FROM pg_stat_activity WHERE pid=$1", &[&pid]).await.expect("independent transaction-end observation").get(0);
                            assert!(idle, "{kind}");
                        }
                        let lease = context.connections().lease().await.expect("next pool borrower");
                        let next_pid: i32 = lease.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("next backend").get(0);
                        assert_eq!(next_pid == pid, !failed, "{kind}");
                        pid = next_pid;
                        lease.release();
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 1);
                    }
                }
                held.release();
                observer.terminate().await.expect("observer ended");
                services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await; proxy.stop().await;
            }).await.expect("rejection connection lifetime budget");
        });
    }

    #[test]
    fn absent_registration_readback_keeps_exact_cleanup_failures_and_incomplete_reads_unknown() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::{
            database::transaction::EndError, identity::authentication::proxied, registration::Error,
        };

        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("driver observation runtime").block_on(async {
            for fault in [Fault::LostReadCommit, Fault::StalledReadCommit, Fault::ReadCommitTermination, Fault::StalledQuery] {
                timeout(Duration::from_secs(35), async {
                    let database = Database::new(Arc::clone(&server)).await;
                    account(&database, "13900000000", false).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = registration_services(proxied(&database, &proxy), &supplier, 4).await;
                    let message_id = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                    let credential = match services.authentication().create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE)).await.expect("R1 registration") {
                        Receipt::Registration(credential) => credential,
                        Receipt::Session(_) => panic!("unregistered mobile"),
                    };
                    let operation = Uuid::new_v4().to_string().parse().expect("operation");
                    let command = registration_input(operation, &credential, "00000020000229007X");
                    proxy.arm(Fault::DelayedRollback);
                    let worker = services.registration().clone();
                    let task = tokio::spawn(async move { worker.register_input_for_test(command).await });
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("write commit held before rollback");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let gate = database.connections(&database.user).recover().await.expect("test operation gate");
                    let gate_pid: i32 = gate.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("gate backend").get(0);
                    let gate_context = context.clone();
                    let gate_task = tokio::spawn(async move {
                        crate::database::transaction::begin(&gate_context, gate.client(), b"registration/operation", operation).await.expect("gate takes original operation after rollback");
                        gate
                    });
                    let mut backends: Vec<i32> = timeout(Duration::from_secs(3), async {
                        loop {
                            let blockers: Vec<i32> = observer.client().query_one("SELECT pg_catalog.pg_blocking_pids($1)", &[&gate_pid]).await.expect("gate is queued behind original write").get(0);
                            if !blockers.is_empty() { break blockers; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("gate waits for original backend");
                    proxy.release();
                    let gate = timeout(Duration::from_secs(3), gate_task).await.expect("original rollback releases gate").expect("gate task");
                    let recovery_pid = timeout(Duration::from_secs(3), async {
                        loop {
                            if let Some(row) = observer.client().query_opt("SELECT pid FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND $1=ANY(pg_catalog.pg_blocking_pids(pid))", &[&gate_pid]).await.expect("actual recovery lock wait") {
                                break row.get::<_, i32>(0);
                            }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("recovery waits for the operation gate");
                    backends.push(recovery_pid);
                    let stalled = matches!(fault, Fault::StalledReadCommit);
                    let terminated = matches!(fault, Fault::ReadCommitTermination);
                    let incomplete = matches!(fault, Fault::StalledQuery);
                    proxy.arm(fault);
                    let recovery = tokio::time::Instant::now();
                    gate.client().batch_execute("ROLLBACK").await.expect("release operation gate");
                    gate.terminate().await.expect("gate ended");
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("recovery fault actually intercepted");
                    let result = timeout(Duration::from_secs(12), task).await.expect("original recovery deadline").expect("worker");
                    assert!(recovery.elapsed() < Duration::from_secs(12));
                    assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 0);
                    assert_eq!(count(&database, "SELECT count(*) FROM identity.receipts").await, 0);
                    assert_eq!(count(&database, "SELECT count(*) FROM identity.registrations").await, 1);
                    let error = result.err().expect("no successful registration");
                    if incomplete {
                        match error {
                            Error::Unknown { recovery, .. } => assert!(matches!(*recovery, Error::Timeout(_)), "{recovery:?}"),
                            other => panic!("incomplete read stays unknown: {other:?}"),
                        }
                    } else {
                        assert_eq!(error.failure(), crate::registration::Failure::Unavailable);
                        let Error::NotCommitted { cause } = error else { panic!("locked absence remains not committed: {error:?}"); };
                        let Error::Readback { cause, commit, termination } = *cause else { panic!("retained readback cleanup"); };
                        assert!(matches!(*cause, Error::Timeout(_) | Error::Combined { .. }));
                        if stalled {
                            assert!(matches!(commit, Err(EndError::CommitTimeout(_))));
                            assert!(termination.is_ok());
                        } else if terminated {
                            assert!(commit.is_ok());
                            assert!(matches!(termination, Err(platform::database::Error::Postgres(ref cause)) if cause.code() == Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)));
                        } else {
                            assert!(matches!(commit, Err(EndError::Commit(ref cause)) if cause.0.is_closed() || cause.0.as_db_error().is_some()));
                        }
                    }
                    proxy.release();
                    timeout(Duration::from_secs(3), async {
                        loop {
                            let ended: bool = observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1))", &[&backends]).await.expect("actual original and recovery backend end").get(0);
                            if ended { break; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("both actual backends ended");
                    let replay = services.registration().register_input_for_test(registration_input(operation, &credential, "00000020000229007X")).await.expect("reliable later retry creates once");
                    assert!(replay.created());
                    assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 1);
                    observer.terminate().await.expect("observer ended");
                    services.drain().await.expect("work drained");
                    context.stop().await.expect("runtime stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "recovery, replay and drain make no additional supplier call");
                    supplier.stop().await;
                    proxy.stop().await;
                }).await.expect("absent recovery scenario budget");
            }
        });
    }

    #[test]
    fn completed_registration_survives_recovery_commit_response_loss() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::proxied;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                let database = Database::new(Arc::clone(&server)).await;
                account(&database, "13900000000", false).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = registration_services(proxied(&database, &proxy), &supplier, 4).await;
                let message_id = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                let credential = match services.authentication().create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE)).await.expect("R1 registration") {
                    Receipt::Registration(credential) => credential,
                    Receipt::Session(_) => panic!("unregistered mobile"),
                };
                let operation = Uuid::new_v4().to_string().parse().expect("operation");
                let command = registration_input(operation, &credential, "00000020000229007X");
                proxy.arm(Fault::DelayedCommit);
                let worker = services.registration().clone();
                let task = tokio::spawn(async move { worker.register_input_for_test(command).await });
                timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("original commit held");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                timeout(Duration::from_secs(5), async {
                    loop {
                        let waiting: bool = observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND cardinality(pg_catalog.pg_blocking_pids(pid))>0)", &[]).await.expect("actual recovery lock wait").get(0);
                        if waiting { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("recovery waits for original backend");
                proxy.arm(Fault::LostReadCommit);
                proxy.release();
                timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("recovery COMMIT response lost");
                let result = timeout(Duration::from_secs(5), task).await.expect("recovery bounded").expect("worker");
                let paired: bool = observer.client().query_one("SELECT attempt.status='registered' AND attempt.credential_id=$2 AND receipt.id=attempt.id AND receipt.operation_id=attempt.operation_id AND receipt.proof=attempt.proof AND session.receipt_id=receipt.id AND session.created_at=attempt.accepted_at AND handoff.id=session.id AND NOT EXISTS(SELECT 1 FROM identity.registrations WHERE id=attempt.credential_id) FROM identity.attempts attempt JOIN identity.receipts receipt ON receipt.operation_id=attempt.operation_id JOIN identity.sessions session ON session.receipt_id=receipt.id JOIN identity.handoffs handoff ON handoff.id=session.id WHERE attempt.operation_id=$1", &[&operation, &credential.id()]).await.expect("complete durable graph before replay").get(0);
                assert!(paired);
                let original = result.expect("complete readback remains known despite cleanup failure");
                assert!(original.created());
                proxy.release();
                let replay = services.registration().register_input_for_test(registration_input(operation, &credential, "00000020000229007X")).await.expect("original replay");
                assert!(!replay.created());
                assert_eq!(original.id(), replay.id());
                assert_eq!(original.token().text(), replay.token().text());
                assert_eq!(original.expires_at(), replay.expires_at());
                assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 1);
                observer.terminate().await.expect("observer ended");
                services.drain().await.expect("work drained");
                context.stop().await.expect("runtime stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "recovery, replay and drain make no additional supplier call");
                supplier.stop().await;
                proxy.stop().await;
            }).await.expect("recovery cleanup budget");
        });
    }

    #[test]
    fn registration_commit_faults_preserve_success_and_refusal_without_a_second_admission() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::proxied;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for refusal in [false, true] {
                for fault in [Fault::LostCommit, Fault::DelayedCommit, Fault::RolledBack] {
                    timeout(Duration::from_secs(25), async {
                        let database = Database::new(Arc::clone(&server)).await;
                        account(&database, "13900000000", false).await;
                        let mut supplier = Supplier::new().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = registration_services(proxied(&database, &proxy), &supplier, 4).await;
                        let service = services.authentication();
                        let message_id = message(&database, service, &mut supplier, "13800000000").await;
                        let credential = match service.create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE)).await.expect("R1 registration") {
                            Receipt::Registration(credential) => credential,
                            Receipt::Session(_) => panic!("unregistered mobile"),
                        };
                        let operation = Uuid::new_v4().to_string().parse().expect("operation");
                        let number = if refusal { "000000200002290010" } else { "00000020000229007X" };
                        let command = registration_input(operation, &credential, number);
                        let delayed = matches!(fault, Fault::DelayedCommit);
                        let rolled_back = matches!(fault, Fault::RolledBack);
                        proxy.arm(fault);
                        let worker = services.registration().clone();
                        let task = tokio::spawn(async move { worker.register_input_for_test(command).await });
                        timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("R2 COMMIT really intercepted");
                        let observer = database.connections(&database.user).recover().await.expect("independent observer");
                        if delayed {
                            timeout(Duration::from_secs(5), async {
                                loop {
                                    let blocked: bool = observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND cardinality(pg_catalog.pg_blocking_pids(pid))>0)", &[]).await.expect("recovery lock observation").get(0);
                                    if blocked { break; }
                                    tokio::task::yield_now().await;
                                }
                            }).await.expect("recovery waits for original backend");
                            assert!(!task.is_finished());
                            assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 0);
                        }
                        proxy.release();
                        let result = timeout(Duration::from_secs(5), task).await.expect("bounded recovery").expect("worker");
                        let attempts: i64 = observer.client().query_one("SELECT count(*) FROM identity.attempts WHERE operation_id=$1", &[&operation]).await.expect("durability before replay").get(0);
                        assert_eq!(attempts, i64::from(!rolled_back));
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.receipts").await, i64::from(!rolled_back && !refusal));
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.registrations").await, i64::from(rolled_back || refusal));
                        if rolled_back {
                            assert!(matches!(result, Err(crate::registration::Error::NotCommitted { .. })));
                        } else {
                            let paired: bool = observer.client().query_one("SELECT attempt.credential_id=$2 AND authentication.status='registrable' AND attempt.accepted_at>=authentication.finished_at AND ((attempt.status='id_number_taken' AND receipt.id IS NULL AND registration.id=attempt.credential_id) OR (attempt.status='registered' AND receipt.id=attempt.id AND receipt.operation_id=attempt.operation_id AND receipt.proof=attempt.proof AND session.receipt_id=receipt.id AND session.created_at=attempt.accepted_at AND handoff.id=session.id AND registration.id IS NULL)) FROM identity.attempts attempt JOIN identity.authentications authentication ON authentication.id=attempt.authentication_id LEFT JOIN identity.receipts receipt ON receipt.operation_id=attempt.operation_id LEFT JOIN identity.sessions session ON session.receipt_id=receipt.id LEFT JOIN identity.handoffs handoff ON handoff.id=session.id LEFT JOIN identity.registrations registration ON registration.id=attempt.credential_id WHERE attempt.operation_id=$1", &[&operation, &credential.id()]).await.expect("complete original result before replay").get(0);
                            assert!(paired);
                        }
                        let replay = services.registration().register_input_for_test(registration_input(operation, &credential, number)).await;
                        if refusal {
                            if !rolled_back { assert!(matches!(result, Err(crate::registration::Error::IdNumberTaken))); }
                            assert!(matches!(replay, Err(crate::registration::Error::IdNumberTaken)));
                        } else {
                            let replay = replay.expect("success replay or confirmed rollback retry");
                            assert_eq!(replay.created(), rolled_back);
                            if !rolled_back {
                                let original = result.expect("recovered success");
                                assert!(original.created());
                                assert_eq!(original.id(), replay.id());
                                assert_eq!(original.token().text(), replay.token().text());
                                assert_eq!(original.expires_at(), replay.expires_at());
                            }
                        }
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 1);
                        observer.terminate().await.expect("observer ended");
                        services.drain().await.expect("shared work drained"); context.stop().await.expect("runtime stopped"); supplier.stop().await; proxy.stop().await;
                    }).await.expect("R2 commit scenario budget");
                }
            }
        });
    }

    #[test]
    fn r2_creates_complete_account_and_replays_without_changing_r1() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::new(server).await;
                account(&database, "13900000000", false).await;
                let mut supplier = Supplier::new().await;
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("runtime");
                let services = crate::startup::Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("barrier"), crate::identity::authentication::inventory(keys()),
                    crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(), crate::identity::authentication::unused_client().await,
                ).await.expect("both verified use cases");
                let service = services.authentication();
                let id = message(&database, service, &mut supplier, "13800000000").await;
                let r1 = Uuid::new_v4();
                let credential = match service.create_session(input(r1, id, "13800000000", CODE)).await.expect("registrable") {
                    Receipt::Registration(credential) => credential,
                    Receipt::Session(_) => panic!("new account has only a registration credential"),
                };
                let registration = services.registration();
                let operation = Uuid::new_v4().to_string().parse().expect("operation");
                let request = || crate::registration::Input::new(operation, credential.id(), credential.token().text().parse().expect("original token"), "Fixture Operator".parse().expect("name"), "00000020000229007X".parse().expect("number"));
                let original = registration.register_input_for_test(request()).await.unwrap_or_else(|error| {
                    let mut cause = &error;
                    loop {
                        match cause {
                            super::Error::NotCommitted { cause: nested } | super::Error::Readback { cause: nested, .. } => cause = nested,
                            super::Error::Query(query) => panic!("isolated registration constraint: {:?}", query.0.as_db_error().map(|error| (error.code().code(), error.constraint(), error.message()))),
                            _ => panic!("complete registration: {error:?}"),
                        }
                    }
                });
                assert!(original.created());
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let row = observer.client().query_one("SELECT receipt.id = attempt.id AND receipt.operation_id = attempt.operation_id AND receipt.credential_id = attempt.credential_id AND receipt.proof = attempt.proof AND account.person_id = receipt.person_id AND person.revision_id = receipt.revision_id AND person.sequence = 1 AND appointment.revision_id = receipt.revision_id AND appointment.operator_id = receipt.revision_id AND appointment.sequence = 1 AND appointment.assigned_at = account.created_at AND session.created_at = account.created_at AND attempt.accepted_at = account.created_at AND role.scope = 'institution' AND role.level = 1 AND authentication.status = 'registrable' AND NOT EXISTS (SELECT 1 FROM identity.registrations WHERE id = receipt.credential_id) AS paired FROM identity.receipts receipt JOIN identity.attempts attempt ON attempt.id = receipt.id JOIN identity.accounts account ON account.id = receipt.account_id JOIN identity.persons person ON person.id = receipt.person_id JOIN access.holders holder ON holder.id = account.id JOIN access.appointments appointment ON appointment.id = holder.assignment_id JOIN access.roles role ON role.id = holder.role_id JOIN identity.sessions session ON session.receipt_id = receipt.id JOIN identity.authentications authentication ON authentication.id = session.authentication_id JOIN identity.handoffs handoff ON handoff.id = session.id JOIN identity.avatars avatar ON avatar.account_id = account.id AND avatar.released_at IS NULL WHERE receipt.operation_id = $1 AND session.id = $2", &[&operation, &original.id()]).await.expect("committed complete graph before replay");
                assert!(row.get::<_, bool>("paired"));
                observer.terminate().await.expect("observer ended");
                let replay = registration.register_input_for_test(request()).await.expect("original handoff");
                assert!(!replay.created());
                assert_eq!(original.id(), replay.id());
                assert_eq!(original.token().text(), replay.token().text());
                assert_eq!(original.expires_at(), replay.expires_at());
                assert!(matches!(service.create_session(input(r1, id, "13800000000", CODE)).await, Err(Error::Verification)));
                age_facts(&database, &services, TimeDelta::hours(25)).await;
                let runtime = database.connections(&database.user).recover().await.expect("real runtime permissions");
                services.registration().verify(runtime.client(), context.sample().await.expect("trusted history time")).await.expect("complete aged graph remains valid before cleanup");
                let paired: bool = runtime.client().query_one("SELECT account.created_at=attempt.accepted_at AND appointment.assigned_at=attempt.accepted_at AND session.created_at=attempt.accepted_at AND authentication.finished_at<=attempt.accepted_at FROM identity.attempts attempt JOIN identity.receipts receipt ON receipt.id=attempt.id JOIN identity.accounts account ON account.id=receipt.account_id JOIN access.appointments appointment ON appointment.operation_id=receipt.operation_id AND appointment.person_id=receipt.person_id AND appointment.sequence=1 JOIN identity.sessions session ON session.receipt_id=receipt.id JOIN identity.authentications authentication ON authentication.id=session.authentication_id WHERE attempt.operation_id=$1", &[&operation]).await.expect("independent consistent aging before cleanup").get(0);
                assert!(paired);
                services.registration().retention().run().await.expect("actual R2 cleanup ends elapsed attempt and handoff");
                assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 0);
                assert_eq!(count(&database, "SELECT count(*) FROM identity.receipts").await, 1);
                assert_eq!(count(&database, "SELECT count(*) FROM identity.sessions").await, 1);
                let second_message = message(&database, service, &mut supplier, "13800000001").await;
                let second = match service.create_session(input(Uuid::new_v4(), second_message, "13800000001", CODE)).await.expect("other valid credential") {
                    Receipt::Registration(credential) => credential,
                    Receipt::Session(_) => panic!("different unregistered mobile"),
                };
                let attempt_id = crate::identity::registration::attempt::Id::allocate();
                let material = keys();
                let mobile = Envelope::seal(&material.encryption, &material.mobile, crate::identity::mobile::envelope::Binding::Attempt(attempt_id), &"13800000001".parse().expect("mobile")).expect("independent attempt mobile");
                let now = context.sample().await.expect("trusted acceptance time");
                runtime.client().batch_execute("BEGIN").await.expect("forged conflicting result");
                runtime.client().execute("INSERT INTO identity.attempts (id,operation_id,credential_id,authentication_id,proof,hash,key_id,mobile,accepted_at,status) SELECT $1,$2,id,authentication_id,$3,$4,$5,$6,$7,'id_number_taken' FROM identity.registrations WHERE id=$8", &[&attempt_id, &operation, &second.token().0.hash().as_slice(), &&[0_u8;32][..], &material.input.active().id(), &mobile, &now, &second.id()]).await.expect("well-formed runtime INSERT reaches deferred pairing");
                let error = runtime.client().batch_execute("COMMIT").await.expect_err("retained receipt excludes a different refusal for the original operation");
                assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                assert!(error.as_db_error().expect("constraint cause").message().contains("refusal"));
                assert_eq!(runtime.client().query_one("SELECT count(*) FROM identity.attempts WHERE operation_id=$1", &[&operation]).await.expect("no conflicting original result").get::<_,i64>(0), 0);
                assert_eq!(runtime.client().query_one("SELECT count(*) FROM identity.receipts WHERE operation_id=$1", &[&operation]).await.expect("original receipt survives").get::<_,i64>(0), 1);
                assert_eq!(runtime.client().query_one("SELECT count(*) FROM identity.registrations WHERE id=$1", &[&second.id()]).await.expect("other credential survives").get::<_,i64>(0), 1);
                runtime.terminate().await.expect("runtime fixture ended");
                let owner = database.connections("postgres").recover().await.expect("isolated source corruption");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role = replica").await.expect("fixture transaction");
                owner.client().execute("UPDATE identity.authentications SET app_id = 'different-website' WHERE operation_id = $1", &[&r1]).await.expect("change only authentication source");
                owner.client().batch_execute("COMMIT").await.expect("fixture committed");
                let receipt_id = owner.client().query_one("SELECT id FROM identity.receipts WHERE operation_id = $1", &[&operation]).await.expect("original receipt").get(0);
                let session = crate::identity::session::find_by_receipt(owner.client(), &registration.0.app, crate::history::Read::Locked, receipt_id, now).await.expect("session projection").expect("retained session without handoff");
                assert!(matches!(crate::identity::session::verify(owner.client(), &session, &material.credential, now).await, Err(crate::identity::session::Error::Facts { rule: "session preserves its authentication source and credential lifetime" })));
                owner.terminate().await.expect("fixture ended");
                services.drain().await.expect("shared work drained");
                context.stop().await.expect("runtime ended");
                supplier.stop().await;
            }).await.expect("R2 smoke budget");
        });
    }
    #[test]
    fn canonical_input_matches_the_independent_python_protocol_vector() {
        let input = Input::new(
            "00000000-0000-4000-8000-000000000001"
                .parse()
                .expect("operation"),
            "00000000-0000-4000-8000-000000000002"
                .parse()
                .expect("credential"),
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"
                .parse()
                .expect("token"),
            "  Fixture Operator  ".parse().expect("name"),
            "000000200002290010".parse().expect("number"),
        );
        let key = hmac::Key::new(Zeroizing::new(std::array::from_fn(|index| {
            u8::try_from(index).expect("byte")
        })));
        assert_eq!(
            hex::encode(
                key.sign(&input.encoded().expect("canonical encoding"))
                    .as_ref()
            ),
            "daebc6503017234c945a1a9f011da902c755aad8738de5a43e3050b4b4df8f39"
        );
        assert_eq!(
            hex::encode(input.token.0.hash()),
            "630dcd2966c4336691125448bbb25b4ff412a49c732db2c8abc1b8581bd710dd"
        );
    }
}
