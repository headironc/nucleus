//! The only entry for write transactions: READ COMMITTED with a deadline, the shared runtime
//! barrier, the instance generation fence and the confirmed original-operation lock.

use std::future::Future;

use chrono::{DateTime, Utc};
use platform::{
    cryptography::{self, Encoded, sha256::Digest},
    database::{Lease, Session},
};
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Client;

use crate::{
    database::Query,
    identity::operation,
    kernel::{
        error::{Error as Failure, Fault, FaultKind, Stage},
        operation::Family,
    },
    runtime::Context,
};

/// Every write and recovery transaction starts here and holds the generation row shared.
///
/// A process whose generation was superseded by a later start or deployment command rolls
/// back before any decisive read and closes admission; its original work stays unknown
/// for the next process. Capabilities not yet on the executor call this directly.
pub(crate) async fn begin(
    context: &Context,
    client: &Client,
    family: &[u8],
    operation: operation::Id,
) -> Result<(), Error> {
    begin_current(context, client).await?;
    object(client, family, operation.as_bytes()).await
}

/// Current-state writes retain the runtime fence without an original-operation identity.
pub(crate) async fn begin_current(context: &Context, client: &Client) -> Result<(), Error> {
    client.batch_execute("BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL statement_timeout = '10s'; SET LOCAL idle_in_transaction_session_timeout = '10s'").await?;
    client
        .batch_execute("SELECT pg_catalog.pg_advisory_xact_lock_shared(1314210636, 1)")
        .await?;
    let generation: i64 = client
        .query_one(
            "SELECT generation FROM security.instances WHERE id FOR SHARE",
            &[],
        )
        .await?
        .try_get(0)?;
    if context.generation() != Some(generation) {
        context.close();
        tracing::error!(
            target: "domain::runtime::instance",
            stage = "instance_lock",
            kind = "superseded",
            "a later process superseded this instance generation; admission closed",
        );
        client.batch_execute("ROLLBACK").await?;
        return Err(Error::Superseded);
    }
    Ok(())
}

pub(crate) async fn object(
    client: &impl tokio_postgres::GenericClient,
    domain: &[u8],
    value: &[u8],
) -> Result<(), Error> {
    client
        .query_one(
            "SELECT pg_catalog.pg_advisory_xact_lock($1)",
            &[&key(domain, value)?],
        )
        .await?;
    Ok(())
}

pub(crate) async fn shared(
    client: &impl tokio_postgres::GenericClient,
    domain: &[u8],
    value: &[u8],
) -> Result<(), Error> {
    client
        .query_one(
            "SELECT pg_catalog.pg_advisory_xact_lock_shared($1)",
            &[&key(domain, value)?],
        )
        .await?;
    Ok(())
}

fn key(domain: &[u8], value: &[u8]) -> Result<i64, cryptography::Error> {
    struct Input<'a> {
        domain: &'a [u8],
        value: &'a [u8],
    }
    impl cryptography::Input<2> for Input<'_> {
        fn fields(&self) -> [&[u8]; 2] {
            [self.domain, self.value]
        }
    }
    let digest = Digest::new(&Encoded::new(&Input { domain, value })?);
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&digest.as_ref()[..8]);
    Ok(i64::from_be_bytes(bytes))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("transaction coordination query failed: {0}")]
    Query(#[from] Query),
    #[error("transaction lock encoding failed")]
    Crypto(#[from] cryptography::Error),
    #[error("a later process superseded this instance generation")]
    Superseded,
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(Query(error))
    }
}

impl Error {
    fn into_kind(self) -> FaultKind {
        match self {
            Self::Query(error) => FaultKind::Query(error),
            Self::Crypto(error) => FaultKind::Crypto(error),
            // The fence already closed admission; the outcome stays with the next process.
            Self::Superseded => FaultKind::Superseded,
        }
    }
}

impl<R> From<Error> for Failure<R> {
    fn from(error: Error) -> Self {
        Self::Unavailable(Fault::new(Stage::Command, error.into_kind()))
    }
}

/// The connection a transaction runs on: an online lease or an independent recovery session.
pub(crate) enum Connection {
    Online(Lease),
    Recovery(Session),
}

impl Connection {
    async fn close(self, reusable: bool) -> Result<(), platform::database::Error> {
        match self {
            Self::Online(lease) if reusable => {
                lease.release();
                Ok(())
            }
            Self::Online(lease) => lease.discard().await,
            Self::Recovery(session) => session.terminate().await,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BeginError {
    #[error(transparent)]
    Coordination(#[from] Error),
    #[error("transaction begin budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
}

impl BeginError {
    fn into_fault(self) -> Fault {
        let kind = match self {
            Self::Coordination(error) => error.into_kind(),
            Self::Timeout(elapsed) => FaultKind::Timeout(elapsed),
        };
        Fault::new(Stage::Begin, kind)
    }
}

/// One write or recovery transaction with its absolute deadline.
pub(crate) struct Tx {
    connection: Connection,
    deadline: Instant,
    operation_id: operation::Id,
}

impl Tx {
    /// Opens the transaction; the original-operation lock is confirmed before any write.
    pub(crate) async fn begin(
        context: &Context,
        connection: Connection,
        deadline: Instant,
        family: Family,
        operation_id: operation::Id,
    ) -> (Self, Result<(), Fault>) {
        let (tx, result) =
            Self::begin_operation(context, connection, deadline, family.lock(), operation_id).await;
        (tx, result.map_err(BeginError::into_fault))
    }

    /// Begins a transaction for an existing operation namespace without inventing a completion
    /// family. Even a failed begin returns its connection owner for explicit cleanup.
    pub(crate) async fn begin_operation(
        context: &Context,
        connection: Connection,
        deadline: Instant,
        family: &[u8],
        operation_id: operation::Id,
    ) -> (Self, Result<(), BeginError>) {
        let tx = Self {
            connection,
            deadline,
            operation_id,
        };
        let result =
            match timeout_at(deadline, begin(context, tx.client(), family, operation_id)).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(BeginError::Coordination(error)),
                Err(elapsed) => Err(BeginError::Timeout(elapsed)),
            };
        (tx, result)
    }

    pub(crate) fn client(&self) -> &Client {
        match &self.connection {
            Connection::Online(lease) => lease.client(),
            Connection::Recovery(session) => session.client(),
        }
    }

    /// The original operation this transaction holds the lock of.
    pub(crate) fn operation_id(&self) -> operation::Id {
        self.operation_id
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) async fn shared<R>(&self, domain: &[u8], value: &[u8]) -> Result<(), Failure<R>> {
        Ok(shared(self.client(), domain, value).await?)
    }

    /// The decisive database observation follows the locks and advances the process clock.
    pub(crate) async fn now<R>(&self, context: &Context) -> Result<DateTime<Utc>, Failure<R>> {
        context.now(self.client()).await.map_err(Failure::clock)
    }

    /// Commits within the deadline. Only a confirmed end returns an online lease to the pool.
    pub(crate) async fn commit(self) -> Result<(), Fault> {
        let (completion, cleanup) = self.complete(Outcome::Pending(())).await;
        let end = match completion {
            Completion::Known(known) => known.end,
            Completion::Pending { error, .. } => Err(error),
        };
        ended(end, cleanup)
    }

    pub(crate) async fn rollback(self) -> Result<(), Fault> {
        let deadline = self.deadline;
        let (known, cleanup) = self.reject((), deadline).await;
        ended(known.end, cleanup)
    }

    /// Ends a write with its outcome and independently closes the connection.
    ///
    /// Only a confirmed COMMIT returns an online lease. A known result survives a failed end and
    /// the caller keeps the separate connection result.
    pub(crate) async fn complete<T>(
        self,
        outcome: Outcome<T>,
    ) -> (Completion<T>, Result<(), platform::database::Error>) {
        let completion = {
            let client = self.client();
            commit(outcome, self.deadline, client.batch_execute("COMMIT")).await
        };
        completion.close(self.connection).await
    }

    /// Confirms an already established result with a read-only COMMIT and closes the connection
    /// independently of that result.
    pub(crate) async fn confirm<T>(
        self,
        value: T,
    ) -> (Known<T>, Result<(), platform::database::Error>) {
        let known = {
            let client = self.client();
            confirm(value, self.deadline, client.batch_execute("COMMIT")).await
        };
        known.close(self.connection).await
    }

    /// Rolls back a determined refusal within its own budget and closes the connection
    /// independently of that refusal.
    pub(crate) async fn reject<T>(
        self,
        value: T,
        deadline: Instant,
    ) -> (Known<T>, Result<(), platform::database::Error>) {
        let known = {
            let client = self.client();
            rollback(value, deadline, client.batch_execute("ROLLBACK")).await
        };
        known.close(self.connection).await
    }

    /// Removes a connection whose transaction state is not established and awaits its driver.
    pub(crate) async fn discard(self) -> Result<(), Fault> {
        self.abandon().await.map_err(terminated)
    }

    /// Removes a connection whose transaction state is not established, retaining the raw
    /// connection cause for the caller's own diagnostic.
    pub(crate) async fn abandon(self) -> Result<(), platform::database::Error> {
        self.connection.close(false).await
    }
}

fn ended(
    end: Result<(), EndError>,
    cleanup: Result<(), platform::database::Error>,
) -> Result<(), Fault> {
    let cleanup = cleanup.map_err(terminated);
    match end {
        Ok(()) => cleanup,
        Err(error) => {
            let (stage, kind) = match error {
                EndError::Commit(error) => (Stage::Commit, FaultKind::Query(error)),
                EndError::CommitTimeout(elapsed) => (Stage::Commit, FaultKind::Timeout(elapsed)),
                EndError::Rollback(error) => (Stage::Rollback, FaultKind::Query(error)),
                EndError::RollbackTimeout(elapsed) => {
                    (Stage::Rollback, FaultKind::Timeout(elapsed))
                }
            };
            Err(Fault::new(stage, kind).then_result(cleanup))
        }
    }
}

fn terminated(error: platform::database::Error) -> Fault {
    Fault::new(Stage::Termination, FaultKind::Connection(error))
}

/// The owning use case establishes an original result, reliable absence, or completed no-op.
/// A pending value describes a completed write body, not a confirmed commit.
/// Downstream callers can supply a write candidate, but cannot declare business facts known.
///
/// ```
/// use domain::database::transaction::Outcome;
/// let candidate = Outcome::Pending(());
/// assert!(matches!(candidate, Outcome::Pending(())));
/// ```
///
/// ```compile_fail
/// use domain::database::transaction::Outcome;
/// let known = Outcome::Known(());
/// ```
#[must_use]
pub enum Outcome<T> {
    #[non_exhaustive]
    Known(T),
    Pending(T),
}

/// A failed transaction end cannot erase known facts or confirm a pending write.
#[must_use]
pub enum Completion<T> {
    Known(Known<T>),
    Pending { value: T, error: EndError },
}

impl<T> Completion<T> {
    /// Always await the independent driver's actual termination.
    pub async fn terminate(
        self,
        session: Session,
    ) -> (Self, Result<(), platform::database::Error>) {
        self.close(Connection::Recovery(session)).await
    }

    /// Confirmed transaction ends permit reuse; all other leases are discarded and awaited.
    /// Successful reuse does not mean that the pooled driver's lifetime has ended.
    pub async fn recycle(self, lease: Lease) -> (Self, Result<(), platform::database::Error>) {
        self.close(Connection::Online(lease)).await
    }

    async fn close(self, connection: Connection) -> (Self, Result<(), platform::database::Error>) {
        let reusable = matches!(&self, Self::Known(Known { end: Ok(()), .. }));
        let cleanup = connection.close(reusable).await;
        (self, cleanup)
    }
}

/// Complete evidence remains known even when ending its transaction fails.
#[must_use]
pub struct Known<T> {
    pub value: T,
    pub end: Result<(), EndError>,
}

impl<T> Known<T> {
    pub async fn terminate(
        self,
        session: Session,
    ) -> (Self, Result<(), platform::database::Error>) {
        self.close(Connection::Recovery(session)).await
    }

    pub async fn recycle(self, lease: Lease) -> (Self, Result<(), platform::database::Error>) {
        self.close(Connection::Online(lease)).await
    }

    async fn close(self, connection: Connection) -> (Self, Result<(), platform::database::Error>) {
        let cleanup = connection.close(self.end.is_ok()).await;
        (self, cleanup)
    }
}

pub async fn commit<T>(
    outcome: Outcome<T>,
    deadline: Instant,
    command: impl Future<Output = Result<(), tokio_postgres::Error>>,
) -> Completion<T> {
    let end = commit_result(deadline, command).await;
    match (outcome, end) {
        (Outcome::Known(value), end) => Completion::Known(Known { value, end }),
        (Outcome::Pending(value), Ok(())) => Completion::Known(Known { value, end: Ok(()) }),
        (Outcome::Pending(value), Err(error)) => Completion::Pending { value, error },
    }
}

/// End a read after its owner has established the value. This does not verify facts or authorize effects.
pub(crate) async fn confirm<T>(
    value: T,
    deadline: Instant,
    command: impl Future<Output = Result<(), tokio_postgres::Error>>,
) -> Known<T> {
    Known {
        value,
        end: commit_result(deadline, command).await,
    }
}

async fn commit_result(
    deadline: Instant,
    command: impl Future<Output = Result<(), tokio_postgres::Error>>,
) -> Result<(), EndError> {
    match timeout_at(deadline, command).await {
        Ok(result) => result.map_err(Query).map_err(EndError::Commit),
        Err(elapsed) => Err(EndError::CommitTimeout(elapsed)),
    }
}

/// Rollback preserves a previously established value; it never confirms a pending write.
pub async fn rollback<T>(
    value: T,
    deadline: Instant,
    command: impl Future<Output = Result<(), tokio_postgres::Error>>,
) -> Known<T> {
    let end = match timeout_at(deadline, command).await {
        Ok(result) => result.map_err(Query).map_err(EndError::Rollback),
        Err(elapsed) => Err(EndError::RollbackTimeout(elapsed)),
    };
    Known { value, end }
}

#[derive(Debug, thiserror::Error)]
pub enum EndError {
    #[error("transaction commit failed: {0}")]
    Commit(#[source] Query),
    #[error("transaction commit budget expired")]
    CommitTimeout(#[source] tokio::time::error::Elapsed),
    #[error("transaction rollback failed: {0}")]
    Rollback(#[source] Query),
    #[error("transaction rollback budget expired")]
    RollbackTimeout(#[source] tokio::time::error::Elapsed),
}
