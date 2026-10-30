use bytes::BytesMut;
use chrono::{DateTime, Utc};
use strum::AsRefStr;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_postgres::{
    Client,
    types::{IsNull, ToSql, Type, to_sql_checked},
};

use crate::{
    database::transaction::{Completion, EndError, Known, Outcome},
    identity::{
        authentication::{
            self, Error, Inner, Input, Receipt, Sender, Stage, TRANSACTION_BUDGET, database,
            transaction,
        },
        challenge::{client, response},
    },
    kernel::tx::{Connection, Tx},
};

pub(super) struct Action;

impl authentication::Execute for Action {
    type Output = Receipt;

    async fn execute(self, service: &Inner, input: &Input) -> Result<Receipt, Error> {
        let verification = service.verify_challenge(input).await?;
        let receipt = verification.receipt();
        verification.finish(input, Ok(receipt))
    }
}

/// Internal stage delivery; a receipt and its diagnostics are not a sending permit.
pub(super) struct Verification {
    receipt: Receipt,
    diagnostic: Option<Box<Error>>,
}

impl Verification {
    fn new(receipt: Receipt) -> Self {
        Self {
            receipt,
            diagnostic: None,
        }
    }

    fn recovered(service: &Inner, receipt: Receipt, diagnostic: Error) -> Self {
        if diagnostic.unreliable() {
            service.registry.close();
        }
        Self {
            receipt,
            diagnostic: Some(Box::new(diagnostic)),
        }
    }

    pub(super) fn receipt(&self) -> Receipt {
        self.receipt
    }

    pub(super) fn diagnostic(&self) -> Option<&Error> {
        self.diagnostic.as_deref()
    }

    pub(super) fn allow_send(self) -> Result<Self, Error> {
        match self.diagnostic {
            Some(cause) if cause.unreliable() => Err(*cause),
            option => Ok(Self {
                receipt: self.receipt,
                diagnostic: option,
            }),
        }
    }

    fn refuse(self, cause: Error) -> Error {
        match self.diagnostic {
            Some(challenge) => Error::ChallengeConclusion {
                cause: Box::new(cause),
                challenge,
            },
            None => cause,
        }
    }

    pub(super) fn finish<T>(self, input: &Input, result: Result<T, Error>) -> Result<T, Error> {
        match result {
            Ok(result) => {
                if let Some(cause) = self.diagnostic {
                    tracing::error!(operation_id = %input.operation_id, stage = "challenge_completion", outcome = self.receipt.status.as_ref(), error = ?cause);
                }
                Ok(result)
            }
            Err(cause) => Err(self.refuse(cause)),
        }
    }
}

enum Transition {
    Original(Receipt),
    Changed(Receipt),
}

/// A durable stage, not an independently constructible verification credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum Status {
    Calling,
    Retryable {
        finished_at: DateTime<Utc>,
    },
    Passed {
        solved_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
    },
    Rejected {
        finished_at: DateTime<Utc>,
    },
    Inconclusive {
        finished_at: DateTime<Utc>,
    },
}

impl Status {
    fn complete(self) -> bool {
        matches!(
            self,
            Self::Passed { .. } | Self::Rejected { .. } | Self::Inconclusive { .. }
        )
    }
    pub(super) fn finished_at(self) -> Option<DateTime<Utc>> {
        match self {
            Self::Calling => None,
            Self::Retryable { finished_at }
            | Self::Passed { finished_at, .. }
            | Self::Rejected { finished_at }
            | Self::Inconclusive { finished_at } => Some(finished_at),
        }
    }
    pub(super) fn solved_at(self) -> Option<DateTime<Utc>> {
        match self {
            Self::Passed { solved_at, .. } => Some(solved_at),
            _ => None,
        }
    }
}

impl ToSql for Status {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().to_sql(kind, out)
    }
    fn accepts(kind: &Type) -> bool {
        <&str as ToSql>::accepts(kind)
    }
    to_sql_checked!();
}

enum Change<'a> {
    Retry,
    Abandon,
    Finish(&'a Result<response::Context, client::Error>),
}

enum Acceptance {
    Existing(Receipt),
    Acquired(Receipt),
    Finished(Verification),
}

impl Inner {
    async fn accept_challenge(&self, input: &Input) -> Result<Acceptance, Error> {
        let lease = self.context.connections().lease().await?;
        let mut inserted = false;
        let (tx, started) = Tx::begin_operation(
            &self.context,
            Connection::Online(lease),
            Instant::now() + TRANSACTION_BUDGET,
            b"challenge/operation",
            input.operation_id,
        )
        .await;
        let result = timeout_at(tx.deadline(), async {
            started?;
            let client = tx.client();
            let original = database::challenge::find_receipt(client, &self.keys, input).await?;
            input.lock_scope(client).await?;
            let receipt = match original {
                Some(receipt) => {
                    self.authenticate_sender(client, input).await?;
                    receipt
                }
                None => {
                    transaction::token(client, &input.token).await?;
                    let subject = match &input.sender {
                        Sender::Anonymous => database::challenge::Subject::Anonymous,
                        Sender::Current(actor) => {
                            let contact = self.contact(client, actor, &input.mobile).await?;
                            database::challenge::Subject::Current {
                                person_id: contact.person_id(),
                                session_id: actor.session_id(),
                            }
                        }
                        Sender::Transfer { actor, transfer_id } => {
                            let contact = self
                                .transfer_contact(client, actor, *transfer_id, &input.mobile)
                                .await?;
                            database::challenge::Subject::Transfer {
                                person_id: contact.person_id(),
                                session_id: actor.session_id(),
                                transfer_id: *transfer_id,
                            }
                        }
                    };
                    let now = self.now(client).await?;
                    let receipt =
                        database::challenge::insert(client, &self.keys, input, subject, now)
                            .await?;
                    inserted = true;
                    receipt
                }
            };
            Ok::<_, Error>(if receipt.status.complete() {
                Outcome::Known(receipt)
            } else {
                Outcome::Pending(receipt)
            })
        })
        .await;
        let error = match result {
            Ok(Ok(outcome)) => {
                let (completion, cleanup) = tx.complete(outcome).await;
                match completion {
                    Completion::Known(Known {
                        value: receipt,
                        end,
                    }) => {
                        let error = match end {
                            Ok(()) => {
                                return Ok(if inserted {
                                    Acceptance::Acquired(receipt)
                                } else {
                                    Acceptance::Existing(receipt)
                                });
                            }
                            Err(error) => Error::from(error).combine(cleanup),
                        };
                        return Ok(Acceptance::Finished(Verification::recovered(
                            self, receipt, error,
                        )));
                    }
                    Completion::Pending { error, .. } => Error::from(error).combine(cleanup),
                }
            }
            Ok(Err(error)) if error.rejection() => {
                let (completion, cleanup) =
                    tx.reject(error, Instant::now() + TRANSACTION_BUDGET).await;
                return Err(transaction::refusal(completion).combine(cleanup));
            }
            Ok(Err(error)) => error.combine(tx.abandon().await),
            Err(error) => Error::from(error).combine(tx.abandon().await),
        };
        // No writes or supplier calls are permitted during this original-operation readback.
        let recovery = match self.recover_challenge(input).await {
            // This task has not called the supplier. Recovering its confirmed first claim
            // preserves ownership; a later caller does not inherit it.
            Ok((
                Known {
                    value: Some(receipt),
                    end: commit,
                },
                termination,
            )) => {
                if receipt.status.complete() {
                    return Ok(Acceptance::Finished(
                        if error.unreliable() || commit.is_err() || termination.is_err() {
                            Verification::recovered(
                                self,
                                receipt,
                                Error::ChallengeReadback {
                                    cause: Box::new(error),
                                    commit,
                                    termination,
                                },
                            )
                        } else {
                            Verification::new(receipt)
                        },
                    ));
                }
                if let Err(recovery) = completed(commit, termination) {
                    return Err(Error::Unknown {
                        cause: Box::new(error),
                        recovery: Box::new(recovery),
                    });
                }
                if error.unreliable() {
                    return Err(error);
                }
                return Ok(if inserted {
                    Acceptance::Acquired(receipt)
                } else {
                    Acceptance::Existing(receipt)
                });
            }
            Ok((
                Known {
                    value: None,
                    end: commit,
                },
                termination,
            )) => {
                return Err(Error::NotCommitted {
                    cause: Box::new(Error::ChallengeReadback {
                        cause: Box::new(error),
                        commit,
                        termination,
                    }),
                });
            }
            Err(recovery) => recovery,
        };
        if recovery.rejection() {
            return Err(recovery);
        }
        Err(Error::Unknown {
            cause: Box::new(error),
            recovery: Box::new(recovery),
        })
    }

    async fn recover_challenge(
        &self,
        input: &Input,
    ) -> Result<
        (
            Known<Option<Receipt>>,
            Result<(), platform::database::Error>,
        ),
        Error,
    > {
        let session = self.context.connections().recover().await?;
        let (tx, started) = Tx::begin_operation(
            &self.context,
            Connection::Recovery(session),
            Instant::now() + TRANSACTION_BUDGET,
            b"challenge/operation",
            input.operation_id,
        )
        .await;
        let result = timeout_at(tx.deadline(), async {
            started?;
            database::challenge::find_receipt(tx.client(), &self.keys, input).await
        })
        .await;
        match result {
            Ok(Ok(option)) => Ok(tx.confirm(option).await),
            Ok(Err(error)) => Err(error.combine(tx.abandon().await)),
            Err(error) => Err(Error::from(error).combine(tx.abandon().await)),
        }
    }

    pub(super) async fn verify_challenge(&self, input: &Input) -> Result<Verification, Error> {
        let mut receipt = match self.accept_challenge(input).await? {
            Acceptance::Finished(verification) => return Ok(verification),
            // A surviving claim does not prove what the former supplier invocation returned.
            Acceptance::Existing(receipt) if receipt.status == Status::Calling => {
                return self.change(input, receipt, &Change::Abandon).await;
            }
            Acceptance::Existing(receipt) | Acceptance::Acquired(receipt) => receipt,
        };
        if receipt.status.complete() {
            return Ok(Verification::new(receipt));
        }
        let sampled = Instant::now();
        let now = self.context.read_time().await?;
        let remaining = receipt
            .expires_at
            .signed_duration_since(now)
            .to_std()
            .unwrap_or_default();
        let deadline = sampled + remaining;
        loop {
            if matches!(receipt.status, Status::Retryable { .. }) {
                let verification = self.change(input, receipt, &Change::Retry).await?;
                receipt = verification.receipt();
                if matches!(receipt.status, Status::Inconclusive { .. }) {
                    return Err(verification.refuse(client::Error::Expired.into()));
                }
                if receipt.status.complete() {
                    return Ok(verification);
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let verification = self.change(input, receipt, &Change::Abandon).await?;
                return Err(verification.refuse(client::Error::Expired.into()));
            }
            let response = match timeout(
                remaining,
                self.supplier.verify(
                    &input.token,
                    receipt.id,
                    input.purpose(),
                    input.operation_id,
                    remaining,
                ),
            )
            .await
            {
                Ok(response) => response,
                Err(error) => Err(client::Error::Timeout(error)),
            };
            let verification = self
                .change(input, receipt, &Change::Finish(&response))
                .await?;
            receipt = verification.receipt();
            if matches!(receipt.status, Status::Retryable { .. }) {
                continue;
            }
            if let Status::Inconclusive { finished_at } = receipt.status {
                match response {
                    Err(error) => return Err(verification.refuse(error.into())),
                    Ok(context) => {
                        if let Err(error) = context.accept(finished_at) {
                            return Err(verification.refuse(error.into()));
                        }
                    }
                }
            }
            return Ok(verification);
        }
    }

    async fn change(
        &self,
        input: &Input,
        previous: Receipt,
        change: &Change<'_>,
    ) -> Result<Verification, Error> {
        let cause = match self.context.connections().lease().await {
            Ok(lease) => match self
                .write(Connection::Online(lease), input, previous, change)
                .await
            {
                Ok((completion, cleanup)) => match completion {
                    Completion::Known(Known {
                        value: receipt,
                        end: Ok(()),
                    }) => return Ok(Verification::new(receipt)),
                    Completion::Known(Known {
                        value: receipt,
                        end: Err(error),
                    }) => {
                        return Ok(Verification::recovered(
                            self,
                            receipt,
                            Error::from(error).combine(cleanup),
                        ));
                    }
                    Completion::Pending { error, .. } => Error::from(error).combine(cleanup),
                },
                // `write` owns the failed attempt's connection and awaited its driver.
                Err(error) => error,
            },
            Err(error) => error.into(),
        };
        // Keep the response in this worker. The independent retry only writes local facts.
        match self.rewrite(input, previous, change).await {
            Ok((
                Completion::Known(Known {
                    value: receipt,
                    end: commit,
                }),
                termination,
            )) => {
                if receipt.status.complete() {
                    return Ok(
                        if cause.unreliable() || commit.is_err() || termination.is_err() {
                            Verification::recovered(
                                self,
                                receipt,
                                Error::ChallengeReadback {
                                    cause: Box::new(cause),
                                    commit,
                                    termination,
                                },
                            )
                        } else {
                            Verification::new(receipt)
                        },
                    );
                }
                match completed(commit, termination) {
                    Ok(()) if !cause.unreliable() => Ok(Verification::new(receipt)),
                    Ok(()) => Err(cause),
                    Err(recovery) => Err(Error::Unknown {
                        cause: Box::new(cause),
                        recovery: Box::new(recovery),
                    }),
                }
            }
            Ok((Completion::Pending { error, .. }, termination)) => Err(Error::Unknown {
                cause: Box::new(cause),
                recovery: Box::new(Error::from(error).combine(termination)),
            }),
            Err(recovery) => Err(Error::Unknown {
                cause: Box::new(cause),
                recovery: Box::new(recovery),
            }),
        }
    }

    async fn rewrite(
        &self,
        input: &Input,
        previous: Receipt,
        change: &Change<'_>,
    ) -> Result<(Completion<Receipt>, Result<(), platform::database::Error>), Error> {
        let session = self.context.connections().recover().await?;
        self.write(Connection::Recovery(session), input, previous, change)
            .await
    }

    async fn write(
        &self,
        connection: Connection,
        input: &Input,
        previous: Receipt,
        change: &Change<'_>,
    ) -> Result<(Completion<Receipt>, Result<(), platform::database::Error>), Error> {
        let (tx, started) = Tx::begin_operation(
            &self.context,
            connection,
            Instant::now() + TRANSACTION_BUDGET,
            b"challenge/operation",
            input.operation_id,
        )
        .await;
        let result = timeout_at(tx.deadline(), async {
            started?;
            self.transition(tx.client(), input, previous, change).await
        })
        .await;
        match result {
            Ok(Ok(transition)) => {
                let outcome = match transition {
                    Transition::Original(receipt) if receipt.status.complete() => {
                        Outcome::Known(receipt)
                    }
                    Transition::Original(receipt) | Transition::Changed(receipt) => {
                        Outcome::Pending(receipt)
                    }
                };
                Ok(tx.complete(outcome).await)
            }
            Ok(Err(error)) => Err(error.combine(tx.abandon().await)),
            Err(error) => Err(Error::from(error).combine(tx.abandon().await)),
        }
    }

    async fn transition(
        &self,
        client: &Client,
        input: &Input,
        previous: Receipt,
        change: &Change<'_>,
    ) -> Result<Transition, Error> {
        let current = database::challenge::find_receipt(client, &self.keys, input)
            .await?
            .ok_or(Error::Facts {
                stage: Stage::ChallengeTransition,
                rule: "the original challenge must still exist",
            })?;
        if matches!(change, Change::Retry) {
            input.lock_scope(client).await?;
        }
        if current.status.complete()
            || (matches!(change, Change::Retry)
                && current.calls == 2
                && current.status == Status::Calling)
            || (matches!(change, Change::Finish(_))
                && current.calls == previous.calls
                && matches!(current.status, Status::Retryable { .. }))
        {
            Ok(Transition::Original(current))
        } else {
            if current.id != previous.id {
                return Err(Error::Facts {
                    stage: Stage::ChallengeTransition,
                    rule: "challenge identity must match the original receipt",
                });
            }
            if current.calls != previous.calls {
                return Err(Error::Facts {
                    stage: Stage::ChallengeTransition,
                    rule: "call generation must match the previous receipt",
                });
            }
            if current.status != previous.status {
                return Err(Error::Facts {
                    stage: Stage::ChallengeTransition,
                    rule: "status must match the previous receipt",
                });
            }
            let now = self.now(client).await?;
            if now < current.created_at {
                return Err(Error::Time {
                    stage: Stage::ChallengeTransition,
                    rule: "trusted time must not precede created_at",
                });
            }
            if current
                .status
                .finished_at()
                .is_some_and(|finished_at| now < finished_at)
            {
                return Err(Error::Time {
                    stage: Stage::ChallengeTransition,
                    rule: "trusted time must not precede the previous finished_at",
                });
            }
            let eligible = if matches!(change, Change::Retry) {
                match &input.sender {
                    Sender::Anonymous => true,
                    Sender::Current(actor) => {
                        match self.contact(client, actor, &input.mobile).await {
                            Ok(_) => true,
                            Err(error) if error.rejection() => false,
                            Err(error) => return Err(error),
                        }
                    }
                    Sender::Transfer { actor, transfer_id } => {
                        match self
                            .transfer_contact(client, actor, *transfer_id, &input.mobile)
                            .await
                        {
                            Ok(_) => true,
                            Err(error) if error.rejection() => false,
                            Err(error) => return Err(error),
                        }
                    }
                }
            } else {
                true
            };
            let next = advance(
                current,
                if eligible { change } else { &Change::Abandon },
                now,
            )?;
            database::challenge::update(client, input, current, next).await?;
            Ok(Transition::Changed(next))
        }
    }
}

fn completed(
    commit: Result<(), EndError>,
    termination: Result<(), platform::database::Error>,
) -> Result<(), Error> {
    match commit {
        Ok(()) => termination.map_err(Error::Connection),
        Err(error) => Err(Error::from(error).combine(termination)),
    }
}

fn advance(
    mut receipt: Receipt,
    change: &Change<'_>,
    now: DateTime<Utc>,
) -> Result<Receipt, Error> {
    match change {
        Change::Retry
            if matches!(receipt.status, Status::Retryable { .. })
                && receipt.calls == 1
                && now < receipt.expires_at =>
        {
            receipt.calls = 2;
            receipt.status = Status::Calling;
        }
        Change::Retry | Change::Abandon => {
            receipt.status = Status::Inconclusive { finished_at: now }
        }
        Change::Finish(result) => {
            if receipt.status != Status::Calling {
                return Err(Error::Facts {
                    stage: Stage::ChallengeTransition,
                    rule: "a supplier conclusion requires the calling state",
                });
            }
            let retryable = match result {
                Err(error) => error.retryable(),
                Ok(response::Context::SupplierFailed { error_codes }) => {
                    error_codes == &[response::Code::InternalError]
                }
                _ => false,
            };
            if retryable && receipt.calls == 1 && now < receipt.expires_at {
                receipt.status = Status::Retryable { finished_at: now };
            } else {
                receipt.status = Status::Inconclusive { finished_at: now };
                if let Ok(context) = result {
                    match context.accept(now) {
                        Ok(proof) => {
                            receipt.status = Status::Passed {
                                solved_at: proof.solved_at(),
                                finished_at: now,
                            };
                        }
                        Err(response::Error::Mismatch) => {
                            receipt.status = Status::Rejected { finished_at: now }
                        }
                        Err(response::Error::SupplierFailed { error_codes })
                            if !error_codes.is_empty()
                                && error_codes.iter().all(|code| {
                                    matches!(
                                        code,
                                        response::Code::InvalidInputResponse
                                            | response::Code::TimeoutOrDuplicate
                                            | response::Code::MissingInputResponse
                                    )
                                }) =>
                        {
                            receipt.status = Status::Rejected { finished_at: now }
                        }
                        Err(_) => {}
                    }
                }
            }
        }
    }
    Ok(receipt)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(super) mod tests {
    use std::{
        sync::{Arc, Mutex, Weak},
        time::Duration,
    };

    use chrono::{DateTime, Utc};
    use nonempty::NonEmpty;
    use platform::database::{self, Connections, credentials::Password};
    use rustls::RootCertStore;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        runtime::Runtime,
        sync::{OnceCell, mpsc, oneshot},
        task::JoinHandle,
        time::timeout,
    };
    use url::Url;
    use uuid::Uuid;

    use crate::{
        database::{Barrier, transaction::EndError},
        identity::{
            authentication::{
                Error, Input, Keys, Receipt, Service, Stage, Status,
                verification::{Acceptance, Change, Verification},
            },
            challenge::{
                client, response,
                settings::{AllowedHostnames, Settings},
            },
        },
        runtime::Context,
    };

    const OPERATION: &str = "00000000-0000-0000-0000-000000000000";
    const TEMPLATE: &str = "fixture_template";
    const OWNER: &str = "fixture_owner";
    const RUNTIME: &str = "fixture_runtime";
    static SERVER: Mutex<Weak<fixture::Server>> = Mutex::new(Weak::new());
    /// One migrated template per live server; a restarted or private server builds its own.
    static TEMPLATES: Mutex<Vec<Template>> = Mutex::new(Vec::new());

    /// Completes once the server's template has been migrated and closed to connections.
    struct Template {
        server: Weak<fixture::Server>,
        built: Arc<OnceCell<()>>,
    }

    pub(crate) fn server() -> Arc<fixture::Server> {
        let mut shared = SERVER.lock().expect("fixture lock");
        if let Some(server) = shared.upgrade() {
            return server;
        }
        let server = Arc::new(fixture::Server::start());
        *shared = Arc::downgrade(&server);
        server
    }

    fn template(server: &Arc<fixture::Server>) -> Arc<OnceCell<()>> {
        let mut templates = TEMPLATES.lock().expect("template lock");
        // Pruning stopped servers first leaves only live allocations, which no new server reuses.
        templates.retain(|template| template.server.strong_count() > 0);
        let server = Arc::downgrade(server);
        if let Some(template) = templates
            .iter()
            .find(|template| template.server.ptr_eq(&server))
        {
            return Arc::clone(&template.built);
        }
        let built = Arc::new(OnceCell::new());
        templates.push(Template {
            server,
            built: Arc::clone(&built),
        });
        built
    }

    /// Whether dropping the fixture also removes its login roles.
    enum Roles {
        /// Template clones share the fixed roles, which end with their server.
        Shared,
        /// Unique roles belong to one database and are dropped with it.
        Dedicated,
    }

    pub(crate) struct Database {
        server: Arc<fixture::Server>,
        pub(in crate::identity::authentication) name: String,
        pub(crate) owner: String,
        pub(crate) user: String,
        roles: Roles,
    }

    impl Database {
        pub(crate) fn connections(&self, user: &str) -> Connections {
            self.connections_to(&self.name, user)
        }

        fn connections_to(&self, name: &str, user: &str) -> Connections {
            let mut roots = RootCertStore::empty();
            roots
                .add(self.server.certificate().clone())
                .expect("fixture CA");
            Connections::new(
                database::Settings::new(
                    "127.0.0.1".into(),
                    self.server.port(),
                    name.into(),
                    user.into(),
                    2,
                    database::settings::Budgets::default(),
                )
                .expect("settings"),
                Password::new(fixture::PASSWORD.into()).expect("fixture password"),
                roots,
            )
            .expect("connections")
        }

        /// Clone the server's migrated template into a new disposable database.
        pub(crate) async fn new(server: Arc<fixture::Server>) -> Self {
            let template = template(&server);
            let database = Self {
                server,
                name: format!("test_{}", Uuid::new_v4().simple()),
                owner: OWNER.into(),
                user: RUNTIME.into(),
                roles: Roles::Shared,
            };
            template.get_or_init(|| database.build_template()).await;
            let admin = database
                .connections_to("postgres", "postgres")
                .recover()
                .await
                .expect("admin");
            admin
                .client()
                .batch_execute(&format!(
                    "CREATE DATABASE {} TEMPLATE {TEMPLATE}",
                    database.name
                ))
                .await
                .expect("cloned database");
            admin.terminate().await.expect("admin ended");
            // Database-level privileges are not copied from a template.
            database.prepare(&database.name).await;
            database
        }

        /// Create unique roles and migrate a new database without the shared template.
        pub(crate) async fn dedicated(server: Arc<fixture::Server>) -> Self {
            let id = Uuid::new_v4().simple().to_string();
            let database = Self {
                server,
                name: format!("test_{id}"),
                owner: format!("owner_{id}"),
                user: format!("runtime_{id}"),
                roles: Roles::Dedicated,
            };
            database.create(&database.name).await;
            database.migrate(&database.name).await;
            database
        }

        /// Replace any template left by a cancelled build, then close it to connections.
        async fn build_template(&self) {
            let admin = self
                .connections_to("postgres", "postgres")
                .recover()
                .await
                .expect("template admin");
            admin
                .client()
                .batch_execute(&format!("DROP DATABASE IF EXISTS {TEMPLATE} WITH (FORCE)"))
                .await
                .expect("remove incomplete template");
            admin
                .client()
                .batch_execute(&format!("DROP ROLE IF EXISTS {RUNTIME}, {OWNER}"))
                .await
                .expect("remove incomplete template roles");
            admin.terminate().await.expect("template admin ended");
            self.create(TEMPLATE).await;
            self.migrate(TEMPLATE).await;
            let admin = self
                .connections_to("postgres", "postgres")
                .recover()
                .await
                .expect("template admin");
            // Cloning fails while any session is connected to the template.
            admin
                .client()
                .batch_execute(&format!(
                    "ALTER DATABASE {TEMPLATE} WITH ALLOW_CONNECTIONS false"
                ))
                .await
                .expect("close template");
            admin.terminate().await.expect("template admin ended");
        }

        async fn create(&self, name: &str) {
            let admin = self
                .connections_to("postgres", "postgres")
                .recover()
                .await
                .expect("admin");
            admin
                .client()
                .batch_execute(&format!("CREATE DATABASE {name}"))
                .await
                .expect("disposable database");
            for user in [&self.owner, &self.user] {
                let sql: String = admin
                    .client()
                    .query_one(
                        "SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', $1::text, $2::text)",
                        &[user, &fixture::PASSWORD],
                    )
                    .await
                    .expect("quote fixture role")
                    .get(0);
                admin
                    .client()
                    .batch_execute(&sql)
                    .await
                    .expect("fixture role");
            }
            admin.terminate().await.expect("admin ended");
            self.prepare(name).await;
        }

        async fn prepare(&self, name: &str) {
            let admin = self
                .connections_to(name, "postgres")
                .recover()
                .await
                .expect("database admin");
            admin.client().query_one("SELECT set_config('nucleus.deployment_user', $1, false), set_config('nucleus.runtime_user', $2, false)", &[&self.owner, &self.user]).await.expect("identities");
            admin
                .client()
                .batch_execute(include_str!("../../../../deployment/roles.sql"))
                .await
                .expect("role boundary");
            admin
                .client()
                .batch_execute(include_str!("../../../../deployment/prepare.sql"))
                .await
                .expect("prepare");
            admin.terminate().await.expect("end preparation");
        }

        async fn migrate(&self, name: &str) {
            let mut owner = self
                .connections_to(name, &self.owner)
                .recover()
                .await
                .expect("owner");
            owner
                .client()
                .query_one(
                    "SELECT set_config('nucleus.runtime_user', $1, false)",
                    &[&self.user],
                )
                .await
                .expect("runtime identity");
            owner
                .client()
                .batch_execute(
                    "CREATE SCHEMA deployment; REVOKE ALL ON SCHEMA deployment FROM PUBLIC",
                )
                .await
                .expect("history schema");
            let transaction = owner
                .client_mut()
                .transaction()
                .await
                .expect("migration transaction");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V1__keys.sql"
                ))
                .await
                .expect("keys migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V2__challenge_acceptance.sql"
                ))
                .await
                .expect("challenge migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V3__message_sending.sql"
                ))
                .await
                .expect("message migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V4__retention.sql"
                ))
                .await
                .expect("retention migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V5__authentication.sql"
                ))
                .await
                .expect("authentication migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V6__initialization.sql"
                ))
                .await
                .expect("initialization migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V7__registration.sql"
                ))
                .await
                .expect("registration migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V8__transfers.sql"
                ))
                .await
                .expect("transfer migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V9__affiliations.sql"
                ))
                .await
                .expect("affiliation migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V10__invitations.sql"
                ))
                .await
                .expect("invitation migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V11__numbers.sql"
                ))
                .await
                .expect("number migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V12__directories.sql"
                ))
                .await
                .expect("directory migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V13__columns.sql"
                ))
                .await
                .expect("column migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V14__avatar_uploads.sql"
                ))
                .await
                .expect("avatar upload migration");
            transaction
                .batch_execute(&crate::database::permissions::grants())
                .await
                .expect("runtime grants");
            let keys = inventory(keys());
            assert!(matches!(
                crate::keys::database::initialize(keys.metadata(), &transaction)
                    .await
                    .expect("metadata"),
                crate::database::transaction::Outcome::Pending(())
            ));
            transaction.commit().await.expect("migrations commit");
            owner.terminate().await.expect("owner ended");
        }

        pub(in crate::identity::authentication) async fn start(
            &self,
            client: client::Client,
        ) -> (Context, Service) {
            self.start_on(client, self.connections(&self.user)).await
        }

        pub(crate) async fn initialized(server: Arc<fixture::Server>) -> Self {
            let database = Self::new(server).await;
            crate::identity::authentication::session::tests::account(
                &database,
                "13900000000",
                false,
            )
            .await;
            database
        }

        pub(in crate::identity::authentication) async fn start_on(
            &self,
            client: client::Client,
            connections: Connections,
        ) -> (Context, Service) {
            let context = Context::new(connections).await.expect("runtime");
            let barrier = Barrier::begin(context.clone())
                .await
                .expect("checked barrier");
            let service = Service::start(
                barrier,
                keys(),
                crate::startup::Settings::new(
                    4,
                    crate::identity::message::Limits::default(),
                    session_settings(),
                    crate::registration::Limits::default(),
                    avatar_settings(),
                )
                .expect("capacity"),
                client,
                super::super::dispatch::tests::unused_client().await,
            )
            .await
            .expect("service");
            (context, service)
        }

        pub(crate) async fn now(&self) -> DateTime<Utc> {
            let session = self
                .connections(&self.user)
                .recover()
                .await
                .expect("time reader");
            let now = session
                .client()
                .query_one("SELECT clock_timestamp()", &[])
                .await
                .expect("database time")
                .get(0);
            session.terminate().await.expect("time reader ended");
            now
        }
    }

    impl Drop for Database {
        fn drop(&mut self) {
            // A fixture can leave scope inside a Tokio runtime or while a test unwinds.
            // Keep the server alive until this database and any dedicated roles are gone.
            let result = std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("fixture cleanup runtime")
                            .block_on(async {
                                timeout(Duration::from_secs(10), async {
                                    let admin = self
                                        .connections_to("postgres", "postgres")
                                        .recover()
                                        .await
                                        .expect("fixture cleanup connection");
                                    let statement: String = admin.client().query_one(
                                        "SELECT format('DROP DATABASE IF EXISTS %I WITH (FORCE)', $1::text)",
                                        &[&self.name],
                                    ).await.expect("quote owned database").get(0);
                                    admin.client().batch_execute(&statement).await.expect("remove owned database");
                                    let roles = match self.roles {
                                        Roles::Shared => [].as_slice(),
                                        Roles::Dedicated => &[&self.user, &self.owner],
                                    };
                                    for user in roles {
                                        let statement: String = admin.client().query_one(
                                            "SELECT format('DROP ROLE IF EXISTS %I', $1::text)",
                                            &[user],
                                        ).await.expect("quote owned role").get(0);
                                        admin.client().batch_execute(&statement).await.expect("remove owned role");
                                    }
                                    admin.terminate().await.expect("fixture cleanup driver ended");
                                }).await.expect("fixture cleanup budget");
                            });
                    })
                    .join()
            });
            if !std::thread::panicking() {
                result.expect("owned database cleanup completed");
            }
        }
    }

    #[test]
    fn disposable_databases_release_storage_without_ending_other_tests() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let survivor = Database::new(Arc::clone(&server)).await;
            let reader = survivor.connections("postgres").recover().await.expect("independent observer");
            reader.client().batch_execute("CREATE TABLE public.fixture_lifecycle (value text NOT NULL); INSERT INTO public.fixture_lifecycle VALUES ('retained')").await.expect("another active fixture's data");
            for (dedicated, unwinding) in [(true, false), (true, true), (false, false), (false, true)] {
                let database = if dedicated { Database::dedicated(Arc::clone(&server)).await } else { Database::new(Arc::clone(&server)).await };
                let name = database.name.clone();
                let owner = database.owner.clone();
                let user = database.user.clone();
                let connection = database.connections(&database.user).recover().await.expect("connection outlives its fixture");
                let pid: i32 = connection.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("owned backend").get(0);
                assert!(reader.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)", &[&name]).await.expect("database exists before cleanup").get::<_, bool>(0));
                if unwinding {
                    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _database = database;
                        panic!("fixture test failure");
                    }));
                    assert!(panic.is_err(), "the original test failure is preserved");
                } else {
                    drop(database);
                }
                let row = reader.client().query_one(
                    "SELECT NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1) AND (SELECT count(*) FROM pg_roles WHERE rolname IN ($2, $3)) = $5 AND NOT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $4)",
                    &[&name, &owner, &user, &pid, &if dedicated { 0_i64 } else { 2 }],
                ).await.expect("independent cleanup facts");
                assert!(row.get::<_, bool>(0), "only dedicated roles leave with their database");
                // Template clones keep sharing the fixed roles.
                survivor.connections(&survivor.user).recover().await.expect("shared runtime role remains").terminate().await.expect("runtime session ended");
                assert_eq!(reader.client().query_one("SELECT value FROM public.fixture_lifecycle", &[]).await.expect("other fixture remains usable").get::<_, String>(0), "retained");
                assert!(connection.client().query_one("SELECT 1", &[]).await.is_err(), "the destroyed fixture cannot retain a live connection");
                let _ = connection.terminate().await;
            }
            reader.terminate().await.expect("observer ended");
        });
    }

    #[test]
    fn concurrent_fixtures_build_one_template_and_restore_database_privileges() {
        // A private server makes the first fixtures race for its template.
        let server = Arc::new(fixture::Server::start());
        let start = std::sync::Barrier::new(4);
        let databases: Vec<Database> = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        Runtime::new().expect("runtime").block_on(async {
                            timeout(Duration::from_secs(60), Database::new(Arc::clone(&server)))
                                .await
                                .expect("fixture budget")
                        })
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().expect("concurrent fixture"))
                .collect()
        });
        Runtime::new().expect("runtime").block_on(async {
            let mut names: Vec<&str> = databases.iter().map(|database| database.name.as_str()).collect();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), 4, "each fixture owns a distinct clone");
            for database in &databases {
                let admin = database.connections("postgres").recover().await.expect("clone administrator");
                let row = admin.client().query_one(
                    "SELECT NOT has_database_privilege('public', current_database(), 'CONNECT,TEMP') AND NOT has_database_privilege($1, current_database(), 'TEMP') AND has_database_privilege($1, current_database(), 'CONNECT') AND has_database_privilege($2, current_database(), 'CREATE') AND to_regclass('workforce.numbers') IS NOT NULL, (SELECT array_agg(datallowconn) FROM pg_database WHERE datname = $3)",
                    &[&database.user, &database.owner, &TEMPLATE],
                ).await.expect("clone facts");
                assert!(row.get::<_, bool>(0), "the clone is migrated and re-applies database privileges");
                assert_eq!(row.get::<_, Vec<bool>>(1), [false], "one template remains closed to connections");
                admin.terminate().await.expect("clone administrator ended");
            }
        });
    }

    /// Identity uses derived from the fixed test root.
    pub(crate) fn keys() -> Keys {
        crate::keys::tests::fixture().identity
    }

    /// The fixed test root with the given Identity sets, which must derive from that root.
    pub(crate) fn inventory(identity: Keys) -> crate::keys::Keys {
        let mut keys = crate::keys::tests::fixture();
        keys.identity = identity;
        keys
    }

    pub(crate) fn avatar_settings() -> crate::identity::avatar::link::Link {
        crate::identity::avatar::link::Link::new(
            "https://avatars.example.com".into(),
            std::time::Duration::from_secs(86400),
        )
        .expect("avatar settings")
    }

    pub(in crate::identity::authentication) fn input() -> Input {
        Input::new(
            OPERATION.parse().expect("id"),
            "13800000000".parse().expect("mobile"),
            "fictional-token".parse().expect("token"),
            "192.0.2.1".parse().expect("IP"),
        )
    }

    pub(crate) fn session_settings() -> crate::identity::authentication::session::settings::Settings
    {
        crate::identity::authentication::session::settings::Settings::new(
            "website-fixture".parse().expect("app"),
            86400,
            600,
            600,
        )
        .expect("session settings")
    }

    pub(crate) struct Request {
        body: serde_json::Value,
        reply: oneshot::Sender<(u16, String)>,
    }
    impl Request {
        pub(crate) fn body(&self) -> &serde_json::Value {
            &self.body
        }
        pub(crate) fn reply(self, status: u16, body: String) {
            assert!(
                self.reply.send((status, body)).is_ok(),
                "original HTTP attempt is still waiting"
            );
        }
    }

    pub(crate) struct Supplier {
        address: Url,
        requests: mpsc::Receiver<Request>,
        task: JoinHandle<()>,
    }
    impl Supplier {
        pub(crate) async fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("loopback supplier");
            let address = Url::parse(&format!(
                "http://{}/siteverify",
                listener.local_addr().expect("address")
            ))
            .expect("URL");
            let (sender, requests) = mpsc::channel(4);
            let task = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.expect("HTTP connection");
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    let header = loop {
                        let count = socket.read(&mut buffer).await.expect("request headers");
                        assert!(count > 0);
                        bytes.extend_from_slice(&buffer[..count]);
                        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                            break end + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&bytes[..header]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .expect("content length");
                    while bytes.len() < header + length {
                        let count = socket.read(&mut buffer).await.expect("request body");
                        assert!(count > 0);
                        bytes.extend_from_slice(&buffer[..count]);
                    }
                    let body = serde_json::from_slice(&bytes[header..header + length])
                        .expect("fixture JSON request");
                    let (reply, receiver) = oneshot::channel();
                    if sender.send(Request { body, reply }).await.is_err() {
                        break;
                    }
                    let Ok((status, body)) = receiver.await else {
                        break;
                    };
                    let response = format!(
                        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                }
            });
            Self {
                address,
                requests,
                task,
            }
        }
        pub(crate) fn client(&self) -> client::Client {
            client::Client::with_address(
                self.address.clone(),
                Settings::new(
                    AllowedHostnames::new(NonEmpty::new("example.com".into())).expect("hostnames"),
                ),
                "fictional-secret".parse().expect("secret"),
            )
            .expect("client")
        }
        pub(crate) fn mini_client(&self) -> crate::identity::mini::client::Client {
            crate::identity::mini::client::Client::for_test(
                self.address.port().expect("fixture port"),
                "mini-fixture".parse().expect("fixture mini identity"),
                crate::identity::mini::client::Secret::new(zeroize::Zeroizing::new(
                    "fictional-mini-secret".into(),
                ))
                .expect("fixture secret"),
            )
            .expect("fixture mini client")
        }
        pub(crate) async fn request(&mut self) -> Request {
            timeout(Duration::from_secs(5), self.observe())
                .await
                .expect("bounded supplier call")
        }
        pub(crate) async fn observe(&mut self) -> Request {
            self.requests
                .recv()
                .await
                .expect("supplier remains available")
        }
        pub(crate) async fn stop(self) {
            self.task.abort();
            let _ = self.task.await;
        }
    }

    fn success(now: DateTime<Utc>) -> String {
        serde_json::json!({"success":true, "challenge_ts":now.to_rfc3339(), "hostname":"example.com", "action":"send_authentication_code", "cdata":OPERATION}).to_string()
    }
    fn run(service: &Service) -> JoinHandle<Result<Receipt, Error>> {
        let service = service.clone();
        tokio::spawn(async move { service.verify(input()).await })
    }
    async fn stop(context: Context, service: Service) {
        service.drain().await.expect("drain");
        context.stop().await.expect("runtime stopped");
    }

    #[test]
    #[tracing_test::traced_test]
    fn rollback_timeout_preserves_refusal_after_waiter_cancellation() {
        use fixture::database_proxy::{Fault, Proxy};
        use tracing::Instrument;

        use crate::identity::authentication::{Failure, dispatch::tests::proxied};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(35), async {
                for cancelled in [false, true] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, service) = database.start_on(supplier.client(), proxied(&database, &proxy)).await;
                    let original = run(&service);
                    supplier.request().await.reply(200, success(database.now().await));
                    original.await.expect("original worker").expect("original challenge");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let snapshot = "SELECT jsonb_agg(to_jsonb(challenge) ORDER BY id)::text FROM identity.challenges challenge";
                    let before: String = observer.client().query_one(snapshot, &[]).await.expect("original facts").get(0);
                    let held = context.connections().lease().await.expect("occupy other pool slot");
                    let lease = context.connections().lease().await.expect("selected backend");
                    let pid: i32 = lease.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("backend PID").get(0);
                    lease.release();
                    proxy.arm(Fault::StalledRollback);
                    let worker = service.clone();
                    let waiter = tokio::spawn(async move {
                        let mut request = input();
                        request.token = "different-fictional-token".parse().expect("wrong proof");
                        worker.verify(request).await
                    }.in_current_span());
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("exact ROLLBACK is waiting");
                    if cancelled {
                        waiter.abort();
                        assert!(waiter.await.expect_err("waiter cancelled").is_cancelled());
                    } else {
                        let error = timeout(Duration::from_secs(12), waiter).await.expect("rollback budget").expect("worker").expect_err("rollback timeout");
                        assert_eq!(error.failure(), Failure::Unavailable);
                        let Error::Rollback { cause, rollback: EndError::RollbackTimeout(_) } = error else { panic!("real elapsed rollback and refusal required: {error:?}"); };
                        assert!(matches!(*cause, Error::Proof));
                    }
                    timeout(Duration::from_secs(12), service.drain()).await.expect("retained worker finishes after cancelled waiter").expect("drain");
                    let after: String = observer.client().query_one(snapshot, &[]).await.expect("facts before replay").get(0);
                    assert_eq!(before, after);
                    // Local driver termination does not end the backend retained by this fault.
                    assert!(observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("retained backend").get::<_, bool>(0));
                    proxy.release();
                    timeout(Duration::from_secs(2), async {
                        loop {
                            if observer.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("backend lifetime").get::<_, bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("old backend ended");
                    let lease = context.connections().lease().await.expect("replacement backend");
                    assert_ne!(pid, lease.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("replacement PID").get::<_, i32>(0));
                    lease.release();
                    held.release();
                    observer.terminate().await.expect("observer ended");
                    context.stop().await.expect("runtime stopped");
                    assert!(supplier.requests.try_recv().is_err(), "no Siteverify repeat through drain");
                    supplier.stop().await;
                    proxy.stop().await;
                    logs_assert(|lines| {
                        let errors: Vec<_> = lines.iter().filter(|line| line.contains(r#"stage="authentication_completion""#)).collect();
                        assert_eq!(errors.len(), usize::from(cancelled) + 1);
                        for error in errors {
                            assert!(error.contains("RollbackTimeout"));
                            assert!(error.contains("cause: Proof"));
                            assert!(error.contains("rollback: RollbackTimeout(Elapsed("));
                            assert!(error.contains(r#"kind="unavailable""#));
                        }
                        Ok(())
                    });
                }
            }).await.expect("bounded rollback timeout cases");
        });
        for secret in [
            "13800000000",
            "fictional-token",
            "different-fictional-token",
            "fictional-secret",
            fixture::PASSWORD,
        ] {
            assert!(!logs_contain(secret));
        }
    }

    #[test]
    fn persists_http_success_and_replays_it_across_a_restart_without_another_call() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let first = run(&service);
            let request = supplier.request().await;
            assert!(request.body["response"] == "fictional-token");
            let second = run(&service);
            request.reply(200, success(database.now().await));
            let receipt = first.await.expect("worker").expect("passed");
            assert!(matches!(receipt.status(), Status::Passed { .. }));
            assert_eq!(
                second.await.expect("waiter").expect("original result"),
                receipt
            );
            let mut wrong = input();
            wrong.token = "different-token".parse().expect("token");
            assert!(matches!(service.verify(wrong).await, Err(Error::Proof)));
            stop(context, service).await;
            let (context, service) = database.start(supplier.client()).await;
            assert_eq!(
                service.verify(input()).await.expect("restart result"),
                receipt
            );
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn retries_only_after_a_durable_second_claim_with_the_original_identity() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let task = run(&service);
            let first = supplier.request().await;
            let original = first.body.clone();
            first.reply(503, "temporarily unavailable".into());
            let second = supplier.request().await;
            assert!(second.body == original);
            let session = database.connections(&database.user).recover().await.expect("observer");
            let row = session.client().query_one("SELECT calls, status, expires_at - created_at = interval '10 seconds' AS fixed FROM identity.challenges", &[]).await.expect("second claim precedes HTTP");
            assert_eq!(row.get::<_, i16>("calls"), 2);
            assert_eq!(row.get::<_, String>("status"), "calling");
            assert!(row.get::<_, bool>("fixed"));
            session.terminate().await.expect("observer ended");
            second.reply(200, success(database.now().await));
            assert!(matches!(task.await.expect("worker").expect("result").status(), Status::Passed { .. }));
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn a_second_internal_error_is_terminal_and_never_grants_a_third_call() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let task = run(&service);
            for _ in 0..2 {
                supplier.request().await.reply(
                    200,
                    r#"{"success":false,"error-codes":["internal-error"]}"#.into(),
                );
            }
            assert!(matches!(
                task.await.expect("worker"),
                Err(Error::Response(_))
            ));
            let receipt = service.verify(input()).await.expect("original conclusion");
            assert_eq!(receipt.calls, 2);
            assert!(matches!(receipt.status(), Status::Inconclusive { .. }));
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn runtime_can_only_advance_the_claim_and_cannot_reopen_a_conclusion() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            service.0.accept_challenge(&input()).await.expect("durable first claim");
            let session = database.connections(&database.user).recover().await.expect("runtime");
            for sql in [
                "UPDATE identity.challenges SET calls = 2",
                "UPDATE identity.challenges SET status = 'passed'",
                "UPDATE identity.challenges SET status = 'passed', solved_at = clock_timestamp() + interval '1 hour', finished_at = clock_timestamp()",
                "UPDATE identity.challenges SET status = 'rejected', finished_at = created_at - interval '1 second'",
            ] {
                assert_eq!(session.client().batch_execute(sql).await.expect_err("invalid transition").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            }
            session.client().batch_execute("UPDATE identity.challenges SET status = 'retryable', finished_at = clock_timestamp()").await.expect("first retryable result");
            assert_eq!(session.client().batch_execute("UPDATE identity.challenges SET status = 'passed', solved_at = clock_timestamp(), finished_at = clock_timestamp()").await.expect_err("no success without a new claim").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            session.client().batch_execute("UPDATE identity.challenges SET calls = 2, status = 'calling', finished_at = NULL").await.expect("second claim");
            assert_eq!(session.client().batch_execute("UPDATE identity.challenges SET status = 'retryable', finished_at = clock_timestamp()").await.expect_err("second attempt cannot become retryable").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            session.client().batch_execute("UPDATE identity.challenges SET status = 'inconclusive', finished_at = clock_timestamp()").await.expect("close second attempt");
            for sql in [
                "UPDATE identity.challenges SET status = 'calling', finished_at = NULL",
                "UPDATE identity.challenges SET calls = 3",
                "UPDATE identity.challenges SET status = 'passed', solved_at = clock_timestamp(), finished_at = clock_timestamp()",
                "UPDATE identity.challenges SET finished_at = clock_timestamp()",
            ] {
                assert_eq!(session.client().batch_execute(sql).await.expect_err("terminal facts cannot change").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            }
            for values in [
                "'calling'::text, NULL::timestamptz, NULL::timestamptz",
                "'passed'::text, clock_timestamp(), clock_timestamp()",
                "'rejected'::text, NULL::timestamptz, clock_timestamp()",
            ] {
                let sql = format!("SELECT * FROM (SELECT {values}) AS facts(status, solved_at, finished_at)");
                let row = session.client().query_one(&sql, &[]).await.expect("typed row");
                assert!(Status::try_from(&row).is_ok());
            }
            for values in [
                "'calling'::text, NULL::timestamptz, clock_timestamp()",
                "'passed'::text, NULL::timestamptz, clock_timestamp()",
                "'rejected'::text, clock_timestamp(), clock_timestamp()",
                "'unknown'::text, NULL::timestamptz, NULL::timestamptz",
            ] {
                let sql = format!("SELECT * FROM (SELECT {values}) AS facts(status, solved_at, finished_at)");
                let row = session.client().query_one(&sql, &[]).await.expect("invalid stored shape");
                assert!(matches!(Status::try_from(&row), Err(Error::Facts { stage: Stage::ChallengeDecode, .. })));
            }
            session.terminate().await.expect("runtime ended");
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn closes_unreliable_responses_without_retrying_or_reopening_the_challenge() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for case in ["rejection", "configuration", "mixed", "unknown", "unknown-retry", "unknown-rejection", "decode", "large", "future", "expired", "status"] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start(supplier.client()).await;
                let task = run(&service);
                let request = supplier.request().await;
                let (status, body) = match case {
                    "rejection" => (200, r#"{"success":false,"error-codes":["invalid-input-response"]}"#.into()),
                    "configuration" => (200, r#"{"success":false,"error-codes":["invalid-input-secret"]}"#.into()),
                    "mixed" => (200, r#"{"success":false,"error-codes":["internal-error","invalid-input-response"]}"#.into()),
                    "unknown" => (200, r#"{"success":false,"error-codes":["sensitive-marker"]}"#.into()),
                    "unknown-retry" => (200, r#"{"success":false,"error-codes":["internal-error","sensitive-marker"]}"#.into()),
                    "unknown-rejection" => (200, r#"{"success":false,"error-codes":["invalid-input-response","sensitive-marker"]}"#.into()),
                    "decode" => (200, r#"{"success":"sensitive-marker"}"#.into()),
                    "large" => (503, "x".repeat(65_537)),
                    "future" => (200, success(database.now().await + chrono::TimeDelta::seconds(10))),
                    "expired" => (200, success(database.now().await - chrono::TimeDelta::seconds(300))),
                    "status" => (429, "sensitive-marker".into()),
                    _ => unreachable!(),
                };
                request.reply(status, body);
                let result = task.await.expect("worker");
                if case == "rejection" { assert!(matches!(result.expect("rejected result").status(), Status::Rejected { .. })); }
                else {
                    let error = result.expect_err("technical response does not yield success");
                    assert!(!format!("{error:?} {error}").contains("sensitive-marker"));
                }
                let receipt = service.verify(input()).await.expect("durable original result");
                if case == "rejection" {
                    assert!(matches!(receipt.status(), Status::Rejected { .. }));
                } else {
                    assert!(matches!(receipt.status(), Status::Inconclusive { .. }));
                }
                assert_eq!(receipt.calls, 1);
                assert!(supplier.requests.try_recv().is_err());
                stop(context, service).await;
                supplier.stop().await;
            }
        });
    }

    #[test]
    fn restart_closes_an_orphaned_claim_but_can_continue_a_confirmed_retryable_stage() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for retryable in [false, true] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start(supplier.client()).await;
                let Acceptance::Acquired(receipt) = service
                    .0
                    .accept_challenge(&input())
                    .await
                    .expect("first claim committed")
                else {
                    panic!("first acceptance acquires the claim");
                };
                if retryable {
                    let response = Ok(response::Context::SupplierFailed {
                        error_codes: vec![response::Code::InternalError],
                    });
                    service
                        .0
                        .change(&input(), receipt, &Change::Finish(&response))
                        .await
                        .expect("retryable stage committed");
                }
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("restart observer");
                let sql =
                    "SELECT id, created_at, expires_at, calls, status, solved_at, finished_at,
                                  (to_jsonb(challenge) - 'calls' - 'status'
                                      - 'solved_at' - 'finished_at')::text AS original
                           FROM identity.challenges challenge WHERE operation_id = $1";
                let read = async || {
                    timeout(
                        Duration::from_secs(5),
                        observer.client().query_one(sql, &[&input().operation_id()]),
                    )
                    .await
                    .expect("restart observation bounded")
                    .expect("original challenge remains")
                };
                let original = read().await;
                assert_eq!(original.get::<_, i16>("calls"), 1);
                assert_eq!(
                    original.get::<_, &str>("status"),
                    if retryable { "retryable" } else { "calling" }
                );
                assert_eq!(
                    original.get::<_, Uuid>("id").to_string(),
                    receipt.id().to_string()
                );
                assert_eq!(
                    original.get::<_, DateTime<Utc>>("created_at"),
                    receipt.created_at()
                );
                assert_eq!(
                    original.get::<_, DateTime<Utc>>("expires_at"),
                    receipt.expires_at()
                );
                let restart_client = supplier.client();
                let restart = async {
                    timeout(Duration::from_secs(5), stop(context, service))
                        .await
                        .expect("first drain bounded");
                    timeout(Duration::from_secs(5), database.start(restart_client))
                        .await
                        .expect("restart bounded")
                };
                let (context, service) = tokio::select! {
                    biased;
                    _ = supplier.requests.recv() => panic!("restart must not call the supplier"),
                    started = restart => started,
                };
                let task = run(&service);
                if retryable {
                    let request = supplier.request().await;
                    // Hold the response while an independent connection checks the claim.
                    let claimed = read().await;
                    assert_eq!(
                        claimed.get::<_, &str>("original"),
                        original.get::<_, &str>("original")
                    );
                    assert_eq!(claimed.get::<_, &str>("status"), "calling");
                    assert_eq!(claimed.get::<_, i16>("calls"), 2);
                    assert_eq!(claimed.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
                    assert_eq!(claimed.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                    assert_eq!(request.body["response"], "fictional-token");
                    assert_eq!(
                        request.body["idempotency_key"],
                        original.get::<_, Uuid>("id").to_string()
                    );
                    request.reply(200, success(database.now().await));
                }
                let recovery = async {
                    let restored = timeout(Duration::from_secs(5), task)
                        .await
                        .expect("restart work bounded")
                        .expect("worker")
                        .expect("original result");
                    // Inspect before replay so another request cannot repair the conclusion.
                    let stored = read().await;
                    assert_eq!(
                        stored.get::<_, &str>("original"),
                        original.get::<_, &str>("original")
                    );
                    if retryable {
                        assert_eq!(stored.get::<_, &str>("status"), "passed");
                        assert_eq!(stored.get::<_, i16>("calls"), 2);
                        assert_eq!(
                            restored.status(),
                            Status::Passed {
                                solved_at: stored.get("solved_at"),
                                finished_at: stored.get("finished_at"),
                            }
                        );
                        assert_eq!(restored.calls, 2);
                    } else {
                        assert_eq!(stored.get::<_, &str>("status"), "inconclusive");
                        assert_eq!(stored.get::<_, i16>("calls"), 1);
                        assert_eq!(stored.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
                        assert_eq!(
                            restored.status(),
                            Status::Inconclusive {
                                finished_at: stored.get("finished_at"),
                            }
                        );
                        assert_eq!(restored.calls, 1);
                    }
                    assert_eq!(restored.id(), receipt.id());
                    assert_eq!(restored.created_at(), receipt.created_at());
                    assert_eq!(restored.expires_at(), receipt.expires_at());
                    assert_eq!(
                        timeout(Duration::from_secs(5), service.verify(input()))
                            .await
                            .expect("replay bounded")
                            .expect("original conclusion"),
                        restored
                    );
                    timeout(Duration::from_secs(5), stop(context, service))
                        .await
                        .expect("restart drain bounded");
                };
                tokio::select! {
                    biased;
                    _ = supplier.requests.recv() => {
                        panic!("restart recovery, replay and drain must not call again")
                    }
                    () = recovery => {}
                }
                assert!(supplier.requests.try_recv().is_err());
                observer.terminate().await.expect("restart observer ends");
                supplier.stop().await;
            }
        });
    }

    #[test]
    fn terminal_challenge_survives_readback_commit_response_loss() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::dispatch::tests::proxied;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let database = Database::initialized(server).await;
                let proxy = Proxy::start(&database.server).await;
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start_on(supplier.client(), proxied(&database, &proxy)).await;
                let task = run(&service);
                supplier.request().await.reply(200, success(database.now().await));
                let original = task.await.expect("original worker").expect("original passed receipt");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let snapshot = "SELECT to_jsonb(challenge)::text FROM identity.challenges challenge WHERE operation_id=$1";
                let before: String = observer.client().query_one(snapshot, &[&input().operation_id()]).await.expect("original facts").get(0);
                let gate = database.connections(&database.user).recover().await.expect("original operation gate");
                super::transaction::begin(&context, gate.client(), input().operation_id()).await.expect("hold original operation");
                let gate_pid: i32 = gate.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("gate PID").get(0);
                proxy.arm(Fault::RejectedBegin);
                let task = run(&service);
                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("initial read BEGIN failed");
                let recovery_pid = timeout(Duration::from_secs(3), async { loop {
                    // Independent E(challenge/operation, nil UUID) SHA-256 prefix.
                    if let Some(row) = observer.client().query_opt("SELECT pid FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND mode='ExclusiveLock' AND classid=2647903637::oid AND objid=2112932477::oid AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND $1=ANY(pg_catalog.pg_blocking_pids(pid))", &[&gate_pid]).await.expect("exact recovery lock") { break row.get::<_,i32>(0); }
                    tokio::task::yield_now().await;
                }}).await.expect("recovery waits on original operation");
                proxy.arm(Fault::LostReadCommit);
                gate.client().batch_execute("COMMIT").await.expect("release original operation");
                gate.terminate().await.expect("gate ended");
                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("readback COMMIT response lost");
                let result = timeout(Duration::from_secs(3), task).await.expect("bounded readback").expect("worker");
                let after: String = observer.client().query_one(snapshot, &[&input().operation_id()]).await.expect("independent facts before replay").get(0);
                assert_eq!(after, before);
                assert_eq!(result.expect("complete terminal readback remains known"), original);
                proxy.release();
                timeout(Duration::from_secs(3), async { loop {
                    if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)", &[&recovery_pid]).await.expect("backend ended").get::<_,bool>(0) { break; }
                    tokio::task::yield_now().await;
                }}).await.expect("readback backend terminates");
                assert_eq!(service.verify(input()).await.expect("original replay"), original);
                stop(context, service).await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no additional Siteverify through replay and drain");
                observer.terminate().await.expect("observer ended"); supplier.stop().await; proxy.stop().await;
            }).await.expect("terminal readback scenario budget");
        });
    }

    #[test]
    fn terminal_delivery_retains_direct_timeout_and_readback_driver_causes() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::dispatch::tests::proxied;

        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                for kind in ["passed", "rejected", "inconclusive"] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, service) = database.start_on(supplier.client(), proxied(&database, &proxy)).await;
                    let task = run(&service);
                    let body = match kind {
                        "passed" => success(database.now().await),
                        "rejected" => r#"{"success":false,"error-codes":["invalid-input-response"]}"#.into(),
                        _ => r#"{"success":false,"error-codes":["invalid-input-secret"]}"#.into(),
                    };
                    supplier.request().await.reply(200, body);
                    let result = task.await.expect("original worker");
                    if kind == "inconclusive" { assert!(matches!(result, Err(Error::Response(_)))); } else { result.expect("terminal response"); }
                    let original = service.verify(input()).await.expect("original receipt");
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    let snapshot = "SELECT to_jsonb(challenge)::text FROM identity.challenges challenge";
                    let before: String = observer.client().query_one(snapshot, &[]).await.expect("original facts").get(0);
                    let verification = if kind == "inconclusive" {
                        let gate = database.connections(&database.user).recover().await.expect("gate");
                        super::transaction::begin(&context, gate.client(), input().operation_id()).await.expect("original operation gate");
                        let pid = gate.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("gate pid").get(0);
                        proxy.arm(Fault::RejectedBegin);
                        let worker = service.clone();
                        let task = tokio::spawn(async move { worker.0.verify_challenge(&input()).await });
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("online failure");
                        wait_for_operation(observer.client(), pid).await;
                        proxy.arm(Fault::ReadCommitTermination);
                        gate.client().batch_execute("COMMIT").await.expect("release gate");
                        gate.terminate().await.expect("gate ended");
                        let verification = task.await.expect("worker").expect("known terminal");
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("actual read commit termination fault");
                        assert!(matches!(verification.diagnostic.as_deref(), Some(Error::ChallengeReadback { commit: Ok(()), termination: Err(platform::database::Error::Postgres(error)), .. }) if error.code() == Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)));
                        verification
                    } else {
                        proxy.arm(if kind == "passed" { Fault::LostReadCommit } else { Fault::StalledReadCommit });
                        let verification = service.0.verify_challenge(&input()).await.expect("direct known terminal");
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("direct read commit fault");
                        let cause = verification.diagnostic.as_deref().expect("direct cleanup diagnostic");
                        let cause = match cause { Error::Combined { cause, .. } => cause.as_ref(), cause => cause };
                        if kind == "passed" { assert!(matches!(cause, Error::Query(error) if error.0.is_closed())); }
                        else { assert!(matches!(cause, Error::Timeout(_))); }
                        proxy.release();
                        verification
                    };
                    let after: String = observer.client().query_one(snapshot, &[]).await.expect("independent facts before replay").get(0);
                    assert_eq!(before, after);
                    assert_eq!(verification.receipt(), original);
                    verification.finish(&input(), Ok(())).expect("terminal final handling");
                    assert_eq!(service.verify(input()).await.expect("public original replay"), original);
                    if kind == "passed" {
                        // Constructed fatal evidence verifies only propagation and admission, not a real corrupt-storage recovery.
                        let verification = Verification::recovered(&service.0, original, Error::Facts { stage: Stage::ChallengeValidation, rule: "fixture fatal evidence" });
                        assert!(service.0.registry.ready().is_err());
                        assert!(matches!(verification.allow_send(), Err(Error::Facts { .. })));
                    }
                    stop(context, service).await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no Siteverify through replay and drain");
                    observer.terminate().await.expect("observer ended"); supplier.stop().await; proxy.stop().await;
                }
                // Both original expiry categories keep their established transport semantics after combination.
                for (cause, expected) in [(Error::Supplier(client::Error::Expired), crate::identity::authentication::Failure::Unavailable), (Error::Response(response::Error::Expired), crate::identity::authentication::Failure::Verification)] {
                    let elapsed = timeout(Duration::ZERO, std::future::pending::<()>()).await.expect_err("actual elapsed");
                    let error = Error::ChallengeConclusion { cause: Box::new(cause), challenge: Box::new(Error::ChallengeReadback { cause: Box::new(Error::Busy), commit: Err(EndError::CommitTimeout(elapsed)), termination: Ok(()) }) };
                    assert_eq!(error.failure(), expected);
                }
            }).await.expect("bounded terminal cases");
        });
    }

    async fn wait_for_operation(observer: &tokio_postgres::Client, blocker: i32) -> i32 {
        timeout(Duration::from_secs(3), async { loop {
            // Independent E(challenge/operation, nil UUID) SHA-256 prefix.
            if let Some(row) = observer.query_opt("SELECT pid FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND mode='ExclusiveLock' AND classid=2647903637::oid AND objid=2112932477::oid AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND $1=ANY(pg_catalog.pg_blocking_pids(pid))", &[&blocker]).await.expect("exact operation lock") { break row.get(0); }
            tokio::task::yield_now().await;
        }}).await.expect("worker waits on original operation")
    }

    #[test]
    fn incomplete_or_nonterminal_readback_never_grants_a_supplier_call() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::dispatch::tests::proxied;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for kind in ["calling", "retryable", "absent", "incomplete"] {
                timeout(Duration::from_secs(20), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, service) = database.start_on(supplier.client(), proxied(&database, &proxy)).await;
                    if kind == "calling" || kind == "retryable" {
                        let Acceptance::Acquired(receipt) = service.0.accept_challenge(&input()).await.expect("durable claim") else { panic!("acquired claim required") };
                        if kind == "retryable" {
                            service.0.change(&input(), receipt, &Change::Finish(&Ok(response::Context::SupplierFailed { error_codes: vec![response::Code::InternalError] }))).await.expect("retryable fact");
                        }
                    }
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    let snapshot = "SELECT COALESCE(jsonb_agg(to_jsonb(challenge)), '[]'::jsonb)::text FROM identity.challenges challenge";
                    let before: String = observer.client().query_one(snapshot, &[]).await.expect("before").get(0);
                    let gate = database.connections(&database.user).recover().await.expect("gate");
                    super::transaction::begin(&context, gate.client(), input().operation_id()).await.expect("operation gate");
                    let pid = gate.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("gate pid").get(0);
                    proxy.arm(Fault::RejectedBegin);
                    let task = run(&service);
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("online failure");
                    wait_for_operation(observer.client(), pid).await;
                    proxy.arm(if kind == "incomplete" { Fault::StalledQuery } else { Fault::LostReadCommit });
                    gate.client().batch_execute("COMMIT").await.expect("release gate");
                    gate.terminate().await.expect("gate ended");
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("target readback fault");
                    let error = timeout(Duration::from_secs(12), task).await.expect("readback deadline").expect("worker").expect_err("no qualification");
                    match (kind, error) {
                        ("absent", Error::NotCommitted { cause }) => assert!(matches!(*cause, Error::ChallengeReadback { commit: Err(EndError::Commit(error)), .. } if error.0.is_closed())),
                        ("incomplete", Error::Unknown { recovery, .. }) => {
                            let cause = match *recovery { Error::Combined { cause, .. } => *cause, error => error };
                            assert!(matches!(cause, Error::Timeout(_)));
                        }
                        (_, Error::Unknown { recovery, .. }) => {
                            let cause = match *recovery { Error::Combined { cause, .. } => *cause, error => error };
                            assert!(matches!(cause, Error::Query(error) if error.0.is_closed()));
                        }
                        (_, error) => panic!("unexpected classification for {kind}: {error:?}"),
                    }
                    let after: String = observer.client().query_one(snapshot, &[]).await.expect("independent facts before any next operation").get(0);
                    assert_eq!(before, after);
                    proxy.release();
                    stop(context, service).await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no Siteverify through drain");
                    observer.terminate().await.expect("observer ended"); supplier.stop().await; proxy.stop().await;
                }).await.unwrap_or_else(|error| panic!("nonterminal case {kind}: {error}"));
            }
        });
    }

    #[test]
    fn send_waits_for_the_previous_terminal_read_transaction_before_claiming_pnvs() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::{
            authentication::dispatch::tests::{client, proxied, reply},
            message,
        };

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let context = Context::new(proxied(&database, &proxy)).await.expect("context");
                let service = Service::start(Barrier::begin(context.clone()).await.expect("barrier"), keys(), crate::startup::Settings::new(4, message::Limits::default(), session_settings(), crate::registration::Limits::default(), avatar_settings()).expect("settings"), supplier.client(), client(&sms)).await.expect("service");
                let task = run(&service);
                supplier.request().await.reply(200, success(database.now().await));
                let original = task.await.expect("worker").expect("passed");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                proxy.arm(Fault::DelayedReadCommit);
                let worker = service.clone();
                let task = tokio::spawn(async move { worker.send(input()).await });
                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("old read COMMIT held before forwarding");
                let (waiting, held) = waiting_operation(observer.client()).await;
                assert_ne!(waiting, held);
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.messages", &[]).await.expect("no permit before crossing old lock").get::<_,i64>(0), 0);
                assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err(), "no PNVS before old transaction ends");
                assert!(!task.is_finished());
                proxy.release();
                reply(sms.request().await, "sent");
                let sent = task.await.expect("send worker").expect("one message");
                let row = observer.client().query_one("SELECT challenge.id, challenge.status, message.id AS message_id, message.status AS delivery, check_row.id AS check_id, claim.id AS claim_id FROM identity.challenges challenge JOIN identity.messages message ON message.challenge_id=challenge.id JOIN identity.checks check_row ON check_row.id=message.id JOIN identity.claims claim ON claim.id=message.id", &[]).await.expect("independent complete graph before replay");
                assert_eq!(row.get::<_,Uuid>("id").to_string(), original.id().to_string());
                assert_eq!(row.get::<_,&str>("status"), "passed"); assert_eq!(row.get::<_,&str>("delivery"), "sent");
                assert_eq!(row.get::<_,Uuid>("message_id"), row.get::<_,Uuid>("check_id")); assert_eq!(row.get::<_,Uuid>("message_id"), row.get::<_,Uuid>("claim_id"));
                timeout(Duration::from_secs(3), async { loop {
                    if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&held]).await.expect("old backend").get::<_,bool>(0) { break; }
                    tokio::task::yield_now().await;
                }}).await.expect("old backend ends");
                assert_eq!(service.send(input()).await.expect("same sent receipt"), sent);
                stop(context, service).await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err(), "no PNVS repeat through replay and drain");
                observer.terminate().await.expect("observer ended"); supplier.stop().await; sms.stop().await; proxy.stop().await;
            }).await.expect("send fencing budget");
        });
    }

    async fn waiting_operation(observer: &tokio_postgres::Client) -> (i32, i32) {
        timeout(Duration::from_secs(3), async { loop {
            if let Some(row) = observer.query_opt("SELECT pid, (pg_catalog.pg_blocking_pids(pid))[1] AS blocker FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND mode='ExclusiveLock' AND classid=2647903637::oid AND objid=2112932477::oid AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND cardinality(pg_catalog.pg_blocking_pids(pid))>0", &[]).await.expect("exact original operation wait") { break (row.get(0), row.get(1)); }
            tokio::task::yield_now().await;
        }}).await.expect("independent operation waiter")
    }

    #[test]
    fn retained_expiry_errors_survive_cleanup_but_unconfirmed_new_terminal_does_not() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::{Failure, dispatch::tests::proxied};

        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                for kind in ["response_expired", "supplier_expired", "new_unconfirmed"] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, service) = database.start_on(supplier.client(), proxied(&database, &proxy)).await;
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    let task = if kind == "supplier_expired" {
                        let Acceptance::Acquired(receipt) = service.0.accept_challenge(&input()).await.expect("first claim") else { panic!("acquired") };
                        service.0.change(&input(), receipt, &Change::Finish(&Ok(response::Context::SupplierFailed { error_codes: vec![response::Code::InternalError] }))).await.expect("retryable");
                        let owner = database.connections(&database.owner).recover().await.expect("isolated historical fixture");
                        owner.client().batch_execute("BEGIN; ALTER TABLE identity.challenges DISABLE TRIGGER challenges_transition_check; UPDATE identity.challenges SET created_at=created_at-interval '20 seconds', expires_at=expires_at-interval '20 seconds', finished_at=finished_at-interval '20 seconds'; ALTER TABLE identity.challenges ENABLE TRIGGER challenges_transition_check; COMMIT").await.expect("consistent expired retryable fixture");
                        owner.terminate().await.expect("fixture ended");
                        proxy.arm(Fault::DelayedCommit);
                        run(&service)
                    } else {
                        let task = run(&service);
                        let request = supplier.request().await;
                        proxy.arm(Fault::DelayedCommit);
                        request.reply(200, success(database.now().await - chrono::TimeDelta::seconds(if kind == "response_expired" {301} else {0})));
                        task
                    };
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original conclusion intercepted");
                    let (_, original_pid) = waiting_operation(observer.client()).await;
                    proxy.arm(if kind == "new_unconfirmed" { Fault::RolledBack } else { Fault::ReadCommitTermination });
                    if kind == "new_unconfirmed" {
                        let admin = database.connections("postgres").recover().await.expect("isolated backend control");
                        assert!(admin.client().query_one("SELECT pg_terminate_backend($1)", &[&original_pid]).await.expect("roll back held original write").get::<_,bool>(0));
                        timeout(Duration::from_secs(3), async { loop {
                            if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&original_pid]).await.expect("original backend end").get::<_,bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }}).await.expect("original write backend ended before result interpretation");
                        admin.terminate().await.expect("admin ended");
                    }
                    proxy.release();
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("recovery completion fault");
                    let error = task.await.expect("worker").expect_err("original failure retained");
                    let row = observer.client().query_one("SELECT status,calls,solved_at,finished_at FROM identity.challenges", &[]).await.expect("independent facts before replay");
                    if kind == "new_unconfirmed" {
                        assert!(matches!(error, Error::Unknown { .. }));
                        assert_eq!(row.get::<_,&str>("status"), "calling");
                        assert!(row.get::<_,Option<DateTime<Utc>>>("finished_at").is_none());
                    } else {
                        assert_eq!(row.get::<_,&str>("status"), "inconclusive");
                        assert!(row.get::<_,Option<DateTime<Utc>>>("solved_at").is_none());
                        assert_eq!(error.failure(), if kind == "supplier_expired" { Failure::Unavailable } else { Failure::Verification });
                        let Error::ChallengeConclusion { cause, challenge } = error else { panic!("both causes required: {error:?}") };
                        assert!(matches!(*challenge, Error::ChallengeReadback { commit: Ok(()), termination: Err(platform::database::Error::Postgres(ref error)), .. } if error.code()==Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)));
                        if kind == "supplier_expired" { assert!(matches!(*cause, Error::Supplier(client::Error::Expired))); }
                        else { assert!(matches!(*cause, Error::Response(response::Error::Expired))); }
                    }
                    assert_eq!(row.get::<_,i16>("calls"), 1);
                    let receipt = service.verify(input()).await.expect("terminal replay or abandonment of original claim");
                    assert!(matches!(receipt.status(), Status::Inconclusive { .. }));
                    stop(context, service).await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no further Siteverify through replay and drain");
                    observer.terminate().await.expect("observer ended"); supplier.stop().await; proxy.stop().await;
                }
            }).await.expect("bounded conclusion cases");
        });
    }

    #[test]
    fn initial_commit_recovery_only_calls_the_supplier_for_a_confirmed_claim() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for fault in [
                fixture::database_proxy::Fault::LostCommit,
                fixture::database_proxy::Fault::DelayedCommit,
                fixture::database_proxy::Fault::RolledBack,
            ] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = fixture::database_proxy::Proxy::start(&server).await;
                let mut roots = RootCertStore::empty();
                roots.add(proxy.authority()).expect("proxy CA");
                let connections = Connections::new(
                    database::Settings::new(
                        "127.0.0.1".into(),
                        proxy.port(),
                        database.name.clone(),
                        database.user.clone(),
                        2,
                        database::settings::Budgets::default(),
                    )
                    .expect("settings"),
                    Password::new(fixture::PASSWORD.into()).expect("password"),
                    roots,
                )
                .expect("proxy connections");
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start_on(supplier.client(), connections).await;
                let rolled_back = matches!(fault, fixture::database_proxy::Fault::RolledBack);
                let delayed = matches!(fault, fixture::database_proxy::Fault::DelayedCommit);
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("fixture observer");
                let operation_id = input().operation_id();
                let read = || async {
                    timeout(
                        Duration::from_secs(5),
                        observer.client().query_opt(
                            "SELECT id, calls, created_at, expires_at, status, solved_at, finished_at
                             FROM identity.challenges WHERE operation_id = $1",
                            &[&operation_id],
                        ),
                    )
                    .await
                    .expect("claim observation bounded")
                    .expect("claim observation")
                };
                proxy.arm(fault);
                let task = run(&service);
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("initial commit intercepted");
                if delayed {
                    // Independent SHA-256 vector: E("challenge/operation", OPERATION's
                    // nil UUID bytes) starts with 9dd3cd95 7df0ca7d. PostgreSQL exposes
                    // the bigint lock's high/low halves as classid/objid, with objsubid=1.
                    let lock = (2_647_903_637_u32, 2_112_932_477_u32);
                    tokio::select! {
                        biased;
                        _ = supplier.requests.recv() => panic!("unconfirmed claim must not call the supplier"),
                        _ = async {
                            // This isolated database has one C1 worker. Recovery must
                            // cross its original transaction's lock before reading the claim.
                            timeout(Duration::from_secs(5), async {
                                loop {
                                    let waiting: bool = observer
                                        .client()
                                        .query_one(
                                            "SELECT EXISTS (
                                                SELECT 1 FROM pg_catalog.pg_locks
                                                WHERE locktype = 'advisory' AND NOT granted
                                                  AND mode = 'ExclusiveLock'
                                                  AND classid = $1 AND objid = $2 AND objsubid = 1
                                                  AND database = (
                                                      SELECT oid FROM pg_catalog.pg_database
                                                      WHERE datname = current_database()
                                                  )
                                                  AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0
                                            )",
                                            &[&lock.0, &lock.1],
                                        )
                                        .await
                                        .expect("recovery lock observation")
                                        .get(0);
                                    if waiting {
                                        break;
                                    }
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .expect("initial recovery waits for the intercepted transaction");
                            assert!(read().await.is_none(), "original claim remains uncommitted");
                            assert!(!task.is_finished());
                        } => {}
                    }
                }
                proxy.release();
                let option = if rolled_back {
                    None
                } else {
                    let request = supplier.request().await;
                    // Read committed facts directly while the supplier response is held.
                    let original = read().await.expect("claim precedes the supplier call");
                    assert_eq!(original.get::<_, &str>("status"), "calling");
                    assert_eq!(original.get::<_, i16>("calls"), 1);
                    assert_eq!(original.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
                    assert_eq!(original.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                    assert_eq!(
                        original.get::<_, DateTime<Utc>>("expires_at")
                            - original.get::<_, DateTime<Utc>>("created_at"),
                        chrono::TimeDelta::seconds(10)
                    );
                    assert_eq!(
                        request.body["idempotency_key"],
                        original.get::<_, Uuid>("id").to_string()
                    );
                    request.reply(200, success(database.now().await));
                    Some(original)
                };
                tokio::select! {
                    biased;
                    _ = supplier.requests.recv() => panic!("recovery and replay must not add a supplier call"),
                    _ = async {
                        let result = timeout(Duration::from_secs(5), task)
                            .await
                            .expect("recovery bounded")
                            .expect("worker");
                        if let Some(original) = option {
                            let receipt = result.expect("original worker retains its claim");
                            // Inspect before replay so a later request cannot repair the conclusion.
                            let stored = read().await.expect("original claim retained");
                            let id: Uuid = original.get("id");
                            assert_eq!(stored.get::<_, Uuid>("id"), id);
                            assert_eq!(receipt.id().to_string(), id.to_string());
                            assert_eq!(stored.get::<_, i16>("calls"), 1);
                            assert_eq!(receipt.calls, 1);
                            assert_eq!(stored.get::<_, &str>("status"), "passed");
                            assert_eq!(receipt.status(), Status::Passed {
                                solved_at: stored.get("solved_at"),
                                finished_at: stored.get("finished_at"),
                            });
                            for (column, actual) in [
                                ("created_at", receipt.created_at()),
                                ("expires_at", receipt.expires_at()),
                            ] {
                                let expected: DateTime<Utc> = original.get(column);
                                assert_eq!(stored.get::<_, DateTime<Utc>>(column), expected);
                                assert_eq!(actual, expected);
                            }
                            assert_eq!(
                                timeout(Duration::from_secs(5), service.verify(input()))
                                    .await
                                    .expect("replay bounded")
                                    .expect("original conclusion"),
                                receipt
                            );
                        } else {
                            assert!(matches!(result, Err(Error::NotCommitted { .. })));
                            assert!(read().await.is_none(), "rolled-back claim remains absent");
                        }
                        timeout(Duration::from_secs(5), stop(context, service))
                            .await
                            .expect("drain bounded");
                    } => {}
                }
                assert!(supplier.requests.try_recv().is_err());
                observer.terminate().await.expect("end fixture observer");
                supplier.stop().await;
                proxy.stop().await;
            }
        });
    }

    #[test]
    fn retained_supplier_success_survives_lost_commit_delayed_commit_and_rollback() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for fault in [
                fixture::database_proxy::Fault::LostCommit,
                fixture::database_proxy::Fault::DelayedCommit,
                fixture::database_proxy::Fault::RolledBack,
            ] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = fixture::database_proxy::Proxy::start(&server).await;
                let mut roots = RootCertStore::empty();
                roots.add(proxy.authority()).expect("proxy CA");
                let connections = Connections::new(
                    database::Settings::new(
                        "127.0.0.1".into(),
                        proxy.port(),
                        database.name.clone(),
                        database.user.clone(),
                        2,
                        database::settings::Budgets::default(),
                    )
                    .expect("settings"),
                    Password::new(fixture::PASSWORD.into()).expect("password"),
                    roots,
                )
                .expect("proxy connections");
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start_on(supplier.client(), connections).await;
                let observer = database
                    .connections(&database.owner)
                    .recover()
                    .await
                    .expect("fixture observer");
                let task = run(&service);
                let request = supplier.request().await;
                // Keep the supplier's timestamp distinct from the local conclusion time.
                let expected_solved_at = database.now().await - chrono::TimeDelta::seconds(1);
                let delayed = matches!(fault, fixture::database_proxy::Fault::DelayedCommit);
                proxy.arm(fault);
                request.reply(200, success(expected_solved_at));
                let recovery = async {
                    timeout(Duration::from_secs(5), proxy.intercepted())
                        .await
                        .expect("conclusion commit intercepted");
                    if delayed {
                        // Independent SHA-256 vector: E("challenge/operation", OPERATION's
                        // nil UUID bytes) starts with 9dd3cd95 7df0ca7d. PostgreSQL exposes
                        // the bigint lock's high/low halves as classid/objid, with objsubid=1.
                        let lock = (2_647_903_637_u32, 2_112_932_477_u32);
                        timeout(Duration::from_secs(5), async {
                            loop {
                                let waiting: bool = observer
                                    .client()
                                    .query_one(
                                        "SELECT EXISTS (
                                        SELECT 1 FROM pg_catalog.pg_locks
                                        WHERE locktype = 'advisory' AND NOT granted
                                          AND mode = 'ExclusiveLock'
                                          AND classid = $1 AND objid = $2 AND objsubid = 1
                                          AND database = (
                                              SELECT oid FROM pg_catalog.pg_database
                                              WHERE datname = current_database()
                                          )
                                          AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0
                                    )",
                                        &[&lock.0, &lock.1],
                                    )
                                    .await
                                    .expect("recovery lock observation")
                                    .get(0);
                                if waiting {
                                    break;
                                }
                                tokio::task::yield_now().await;
                            }
                        })
                        .await
                        .expect("conclusion recovery waits for the original operation lock");
                        let pending = observer
                            .client()
                            .query_one(
                                "SELECT status, solved_at, finished_at FROM identity.challenges
                             WHERE operation_id = $1",
                                &[&input().operation_id()],
                            )
                            .await
                            .expect("conclusion remains uncommitted");
                        assert_eq!(pending.get::<_, &str>("status"), "calling");
                        assert_eq!(pending.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
                        assert_eq!(pending.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                        assert!(!task.is_finished());
                    }
                    proxy.release();
                    let receipt = timeout(Duration::from_secs(5), task)
                        .await
                        .expect("recovery budget")
                        .expect("worker")
                        .expect("retained success");
                    let Status::Passed {
                        solved_at,
                        finished_at,
                    } = receipt.status()
                    else {
                        panic!("retained supplier success must pass");
                    };
                    assert_eq!(solved_at, expected_solved_at);
                    assert_eq!(receipt.calls, 1);
                    // Read before replay so a later request cannot repair a missing conclusion.
                    let stored = observer
                        .client()
                        .query_one(
                            "SELECT status, calls, solved_at, finished_at FROM identity.challenges
                         WHERE operation_id = $1",
                            &[&input().operation_id()],
                        )
                        .await
                        .expect("original worker persisted its conclusion");
                    assert_eq!(stored.get::<_, &str>("status"), "passed");
                    assert_eq!(stored.get::<_, i16>("calls"), 1);
                    assert_eq!(
                        stored.get::<_, DateTime<Utc>>("solved_at"),
                        expected_solved_at
                    );
                    assert_eq!(stored.get::<_, DateTime<Utc>>("finished_at"), finished_at);
                    assert_eq!(
                        timeout(Duration::from_secs(5), service.verify(input()))
                            .await
                            .expect("replay bounded")
                            .expect("original conclusion"),
                        receipt
                    );
                    observer.terminate().await.expect("end fixture observer");
                    timeout(Duration::from_secs(5), stop(context, service))
                        .await
                        .expect("drain bounded");
                };
                tokio::select! {
                    biased;
                    _ = supplier.requests.recv() => {
                        panic!("conclusion recovery, replay and drain must not call again")
                    }
                    () = recovery => {}
                }
                assert!(supplier.requests.try_recv().is_err());
                supplier.stop().await;
                proxy.stop().await;
            }
        });
    }

    #[test]
    fn transition_begin_deadline_lets_one_independent_rewrite_complete_the_same_result() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::dispatch::tests::proxied;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let mut supplier = Supplier::new().await;
                let (context, service) = database
                    .start_on(supplier.client(), proxied(&database, &proxy))
                    .await;
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("independent observer");
                let task = run(&service);
                let request = supplier.request().await;
                // Keep the supplier's timestamp distinct from the rewrite's decision time.
                let expected_solved_at = database.now().await - chrono::TimeDelta::seconds(1);
                // Hold the transition's first statement, before the shared generation
                // barrier returns. The one absolute budget still starts before BEGIN.
                proxy.arm(Fault::StalledQuery);
                request.reply(200, success(expected_solved_at));
                let stalled = timeout(Duration::from_secs(3), proxy.intercepted())
                    .await
                    .expect("transition begin Parse held");
                let receipt = timeout(Duration::from_secs(15), task)
                    .await
                    .expect("absolute transition budget")
                    .expect("worker")
                    .expect("independently rewritten conclusion");
                let Status::Passed {
                    solved_at,
                    finished_at,
                } = receipt.status()
                else {
                    panic!("retained supplier success must pass");
                };
                assert_eq!(solved_at, expected_solved_at);
                assert_eq!(receipt.calls, 1);
                // Inspect before releasing the original attempt: its held transaction
                // cannot have produced these facts, so the rewrite did.
                let stored = observer
                    .client()
                    .query_one(
                        "SELECT status, calls, solved_at, finished_at FROM identity.challenges
                         WHERE operation_id = $1",
                        &[&input().operation_id()],
                    )
                    .await
                    .expect("persisted conclusion while the original attempt is held");
                assert_eq!(stored.get::<_, &str>("status"), "passed");
                assert_eq!(stored.get::<_, i16>("calls"), 1);
                assert_eq!(
                    stored.get::<_, DateTime<Utc>>("solved_at"),
                    expected_solved_at
                );
                assert_eq!(stored.get::<_, DateTime<Utc>>("finished_at"), finished_at);
                let snapshot = "SELECT to_jsonb(challenge)::text FROM identity.challenges challenge WHERE operation_id = $1";
                let held: String = observer
                    .client()
                    .query_one(snapshot, &[&input().operation_id()])
                    .await
                    .expect("conclusion facts while the original attempt is held")
                    .get(0);
                proxy.release();
                timeout(Duration::from_secs(3), async {
                    loop {
                        if observer
                            .client()
                            .query_one(
                                "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = $1)",
                                &[&stalled],
                            )
                            .await
                            .expect("original backend lifetime")
                            .get::<_, bool>(0)
                        {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("original backend ends after its driver");
                let after: String = observer
                    .client()
                    .query_one(snapshot, &[&input().operation_id()])
                    .await
                    .expect("conclusion facts after the original attempt ends")
                    .get(0);
                assert_eq!(
                    after, held,
                    "the abandoned attempt cannot rewrite the conclusion"
                );
                stop(context, service).await;
                assert!(
                    timeout(Duration::from_millis(30), supplier.request())
                        .await
                        .is_err(),
                    "the independent rewrite must not repeat Siteverify"
                );
                observer.terminate().await.expect("observer ended");
                supplier.stop().await;
                proxy.stop().await;
            })
            .await
            .expect("transition deadline scenario budget");
        });
    }

    #[test]
    fn initial_commit_recovery_observes_persisted_expiry_without_calling_the_supplier() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let owner = database
                .connections(&database.owner)
                .recover()
                .await
                .expect("fixture owner");
            // Backdate only this fixture's persisted facts, preserving their ten-second
            // lifetime. Recovery must use them instead of the fresh in-memory receipt.
            owner
                .client()
                .batch_execute(
                    r#"
                CREATE FUNCTION identity.expire_challenge() RETURNS trigger
                LANGUAGE plpgsql AS $$
                BEGIN
                    NEW.created_at := NEW.created_at - interval '1 hour';
                    NEW.expires_at := NEW.expires_at - interval '1 hour';
                    RETURN NEW;
                END
                $$;
                CREATE TRIGGER challenge_expiry BEFORE INSERT ON identity.challenges
                    FOR EACH ROW EXECUTE FUNCTION identity.expire_challenge()
            "#,
                )
                .await
                .expect("expired claim fixture");
            owner.terminate().await.expect("end fixture owner");
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut roots = RootCertStore::empty();
            roots.add(proxy.authority()).expect("proxy CA");
            let connections = Connections::new(
                database::Settings::new(
                    "127.0.0.1".into(),
                    proxy.port(),
                    database.name.clone(),
                    database.user.clone(),
                    2,
                    database::settings::Budgets::default(),
                )
                .expect("settings"),
                Password::new(fixture::PASSWORD.into()).expect("password"),
                roots,
            )
            .expect("proxy connections");
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start_on(supplier.client(), connections).await;
            let observer = database
                .connections(&database.owner)
                .recover()
                .await
                .expect("fixture observer");
            proxy.arm(fixture::database_proxy::Fault::DelayedCommit);
            let restart_client = supplier.client();
            let task = run(&service);
            let recovery = async {
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("initial commit intercepted");
                // Independent SHA-256 vector: E("challenge/operation", OPERATION's
                // nil UUID bytes) starts with 9dd3cd95 7df0ca7d. PostgreSQL exposes
                // the bigint lock's high/low halves as classid/objid, with objsubid=1.
                let lock = (2_647_903_637_u32, 2_112_932_477_u32);
                timeout(Duration::from_secs(5), async {
                    loop {
                        let waiting: bool = observer
                            .client()
                            .query_one(
                                "SELECT EXISTS (
                                SELECT 1 FROM pg_catalog.pg_locks
                                WHERE locktype = 'advisory' AND NOT granted
                                  AND mode = 'ExclusiveLock'
                                  AND classid = $1 AND objid = $2 AND objsubid = 1
                                  AND database = (
                                      SELECT oid FROM pg_catalog.pg_database
                                      WHERE datname = current_database()
                                  )
                                  AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0
                            )",
                                &[&lock.0, &lock.1],
                            )
                            .await
                            .expect("recovery lock observation")
                            .get(0);
                        if waiting {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("expired claim recovery waits for the original operation lock");
                let row = observer
                .client()
                .query_one(
                    "SELECT clock_timestamp() AS now,
                            NOT EXISTS (SELECT 1 FROM identity.challenges) AS uncommitted",
                    &[],
                )
                .await
                .expect("expiry precedes the delayed commit");
                let held_at: DateTime<Utc> = row.get("now");
                assert!(row.get::<_, bool>("uncommitted"));
                assert!(!task.is_finished());
                proxy.release();
                let result = timeout(Duration::from_secs(5), task)
                    .await
                    .expect("recovery finishes")
                    .expect("worker");
                assert!(matches!(
                    result,
                    Err(Error::Supplier(client::Error::Expired))
                ));
                // Inspect before replay: a later verify could itself close an orphaned claim.
                let stored = observer
                .client()
                .query_one(
                    "SELECT status, finished_at, expires_at FROM identity.challenges WHERE operation_id = $1",
                    &[&input().operation_id()],
                )
                .await
                .expect("original worker persisted its conclusion");
                assert_eq!(stored.get::<_, &str>("status"), "inconclusive");
                let stored_at: DateTime<Utc> = stored.get("finished_at");
                let expires_at: DateTime<Utc> = stored.get("expires_at");
                assert!(expires_at < held_at, "the original deadline expired while COMMIT was held");
                let receipt = timeout(Duration::from_secs(5), service.verify(input()))
                    .await
                    .expect("replay bounded")
                    .expect("original conclusion");
                let Status::Inconclusive { finished_at } = receipt.status() else {
                    panic!("expired call is closed");
                };
                assert_eq!(receipt.expires_at(), expires_at);
                assert_eq!(
                    receipt.expires_at() - receipt.created_at(),
                    chrono::TimeDelta::seconds(10)
                );
                assert!(finished_at >= receipt.expires_at());
                assert_eq!(finished_at, stored_at);
                assert_eq!(receipt.calls, 1);
                observer.terminate().await.expect("end fixture observer");
                timeout(Duration::from_secs(5), stop(context, service))
                    .await
                    .expect("first drain bounded");
                let (context, service) = database.start(restart_client).await;
                assert_eq!(
                    timeout(Duration::from_secs(5), service.verify(input()))
                        .await
                        .expect("restart replay bounded")
                        .expect("restart result"),
                    receipt
                );
                timeout(Duration::from_secs(5), stop(context, service))
                    .await
                    .expect("restart drain bounded");
            };
            tokio::select! {
                biased;
                _ = supplier.requests.recv() => {
                    panic!("expired recovery, replay, restart and drain must not call")
                }
                () = recovery => {}
            }
            assert!(supplier.requests.try_recv().is_err());
            supplier.stop().await;
            proxy.stop().await;
        });
    }

    #[test]
    fn expired_retryable_stage_reports_expiry_once_and_preserves_its_conclusion() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let Acceptance::Acquired(receipt) = service
                .0
                .accept_challenge(&input())
                .await
                .expect("first claim")
            else {
                panic!("first acceptance acquires the claim");
            };
            service
                .0
                .change(
                    &input(),
                    receipt,
                    &Change::Finish(&Ok(response::Context::SupplierFailed {
                        error_codes: vec![response::Code::InternalError],
                    })),
                )
                .await
                .expect("retryable stage");
            stop(context, service).await;
            let owner = database
                .connections(&database.owner)
                .recover()
                .await
                .expect("offline fixture owner");
            owner
                .client()
                .batch_execute(
                    r#"
                BEGIN;
                ALTER TABLE identity.challenges DISABLE TRIGGER challenges_transition_check;
                UPDATE identity.challenges
                SET created_at = created_at - interval '1 hour',
                    expires_at = expires_at - interval '1 hour',
                    finished_at = finished_at - interval '1 hour';
                ALTER TABLE identity.challenges ENABLE TRIGGER challenges_transition_check;
                COMMIT
            "#,
                )
                .await
                .expect("expired retained history fixture");
            owner.terminate().await.expect("end fixture owner");
            let (context, service) = database.start(supplier.client()).await;
            assert!(matches!(
                service.verify(input()).await,
                Err(Error::Supplier(client::Error::Expired))
            ));
            let restored = service.verify(input()).await.expect("original conclusion");
            assert!(matches!(restored.status(), Status::Inconclusive { .. }));
            assert_eq!(restored.id(), receipt.id());
            assert_eq!(restored.calls, 1);
            assert_eq!(
                restored.expires_at(),
                receipt.expires_at() - chrono::TimeDelta::hours(1)
            );
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn exhausted_deadline_does_not_start_another_call_or_reopen_after_restart() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let task = run(&service);
            let request = supplier.request().await;
            let result = timeout(Duration::from_secs(12), task)
                .await
                .expect("original budget")
                .expect("worker");
            assert!(matches!(result, Err(Error::Supplier(_))));
            drop(request);
            let receipt = service.verify(input()).await.expect("original result");
            assert!(matches!(receipt.status(), Status::Inconclusive { .. }));
            assert_eq!(receipt.calls, 1);
            stop(context, service).await;
            let (context, service) = database.start(supplier.client()).await;
            assert_eq!(
                service.verify(input()).await.expect("restart result"),
                receipt
            );
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn two_failed_local_writes_preserve_both_causes_and_do_not_repeat_siteverify() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let task = run(&service);
            let request = supplier.request().await;
            let owner = database
                .connections(&database.owner)
                .recover()
                .await
                .expect("owner");
            let observer = database
                .connections(&database.user)
                .recover()
                .await
                .expect("observer");
            let sql = "SELECT id, created_at, expires_at, status, calls, solved_at, finished_at,
                                      to_jsonb(challenge)::text AS facts,
                                      (to_jsonb(challenge) - 'status' - 'solved_at' - 'finished_at')::text AS original
                               FROM identity.challenges challenge WHERE operation_id = $1";
            let read = async || {
                timeout(
                    Duration::from_secs(5),
                    observer.client().query_one(sql, &[&input().operation_id()]),
                )
                .await
                .expect("fact observation bounded")
                .expect("original challenge remains")
            };
            let original = read().await;
            assert_eq!(original.get::<_, &str>("status"), "calling");
            assert_eq!(original.get::<_, i16>("calls"), 1);
            assert_eq!(original.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
            assert_eq!(
                original.get::<_, Option<DateTime<Utc>>>("finished_at"),
                None
            );
            assert_eq!(
                request.body["idempotency_key"],
                original.get::<_, Uuid>("id").to_string()
            );
            owner
                .client()
                .batch_execute(&format!(
                    "REVOKE UPDATE (status) ON identity.challenges FROM {}",
                    database.user
                ))
                .await
                .expect("inject local write failure");
            request.reply(200, success(database.now().await));
            let recovery = async {
                let error = timeout(Duration::from_secs(5), task)
                    .await
                    .expect("failed writes bounded")
                    .expect("worker")
                    .expect_err("write outcome unresolved");
                let Error::Unknown { cause, recovery } = &error else {
                    panic!("both local write failures remain visible");
                };
                for failure in [cause, recovery] {
                    let Error::Query(query) = failure.as_ref() else {
                        panic!("local write must fail at the database");
                    };
                    assert_eq!(
                        query.code(),
                        Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
                    );
                }
                let diagnostic = format!("{error:?} {error}");
                for secret in ["fictional-token", "fictional-secret", "13800000000"] {
                    assert!(!diagnostic.contains(secret));
                }
                let unresolved = read().await;
                assert_eq!(
                    unresolved.get::<_, &str>("facts"),
                    original.get::<_, &str>("facts")
                );
                owner
                    .client()
                    .batch_execute(&format!(
                        "GRANT UPDATE (status) ON identity.challenges TO {}",
                        database.user
                    ))
                    .await
                    .expect("restore fixture permission");
                let closing_at = database.now().await;
                let receipt = timeout(Duration::from_secs(5), service.verify(input()))
                    .await
                    .expect("orphan recovery bounded")
                    .expect("orphan closed");
                // Inspect before replay so another request cannot repair the conclusion.
                let stored = read().await;
                assert_eq!(
                    stored.get::<_, &str>("original"),
                    original.get::<_, &str>("original")
                );
                assert_eq!(stored.get::<_, &str>("status"), "inconclusive");
                assert_eq!(stored.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
                let finished_at: DateTime<Utc> = stored.get("finished_at");
                assert!(finished_at >= closing_at && finished_at <= database.now().await);
                assert_eq!(receipt.status(), Status::Inconclusive { finished_at });
                assert_eq!(receipt.calls, 1);
                assert_eq!(
                    receipt.id().to_string(),
                    original.get::<_, Uuid>("id").to_string()
                );
                assert_eq!(
                    receipt.created_at(),
                    original.get::<_, DateTime<Utc>>("created_at")
                );
                assert_eq!(
                    receipt.expires_at(),
                    original.get::<_, DateTime<Utc>>("expires_at")
                );
                assert_eq!(
                    timeout(Duration::from_secs(5), service.verify(input()))
                        .await
                        .expect("replay bounded")
                        .expect("original conclusion"),
                    receipt
                );
                owner.terminate().await.expect("owner ended");
                timeout(Duration::from_secs(5), stop(context, service))
                    .await
                    .expect("drain bounded");
            };
            tokio::select! {
                biased;
                _ = supplier.requests.recv() => {
                    panic!("failed writes, recovery and drain must not repeat Siteverify")
                }
                () = recovery => {}
            }
            assert!(supplier.requests.try_recv().is_err());
            observer.terminate().await.expect("observer ended");
            supplier.stop().await;
        });
    }

    #[test]
    fn a_clock_rollback_during_the_call_keeps_the_claim_unresolved_and_closes_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let (context, service) = database
                .start_on(
                    supplier.client(),
                    crate::identity::authentication::proxied(&database, &proxy),
                )
                .await;
            let task = run(&service);
            let request = supplier.request().await;
            let owner = database
                .connections(&database.owner)
                .recover()
                .await
                .expect("owner");
            proxy.arm(fixture::database_proxy::Fault::RewoundCurrent);
            request.reply(200, success(database.now().await));
            timeout(Duration::from_secs(2), proxy.intercepted())
                .await
                .expect("conclusion observes rewound time");
            proxy.release();
            assert!(matches!(
                task.await.expect("worker"),
                Err(Error::Unknown { .. })
            ));
            assert!(matches!(service.verify(input()).await, Err(Error::Closed)));
            assert_eq!(
                owner
                    .client()
                    .query_one("SELECT status FROM identity.challenges", &[])
                    .await
                    .expect("unresolved claim")
                    .get::<_, &str>(0),
                "calling"
            );
            owner.terminate().await.expect("owner ended");
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
            proxy.stop().await;
        });
    }

    #[test]
    fn cancelled_waiter_does_not_release_the_supplier_work_or_its_stop_obligation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let task = run(&service);
            let request = supplier.request().await;
            task.abort();
            let _ = task.await;
            let drain = service.clone();
            let mut drain = tokio::spawn(async move { drain.drain().await });
            assert!(
                timeout(Duration::from_millis(30), &mut drain)
                    .await
                    .is_err()
            );
            request.reply(200, success(database.now().await));
            timeout(Duration::from_secs(5), drain)
                .await
                .expect("supplier work ends")
                .expect("drain task")
                .expect("drain");
            context.stop().await.expect("stop context");
            let (context, service) = database.start(supplier.client()).await;
            assert!(matches!(
                service
                    .verify(input())
                    .await
                    .expect("original outcome")
                    .status(),
                Status::Passed { .. }
            ));
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }
}
