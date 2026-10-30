//! Closed diagnostics for startup budgets and bounded retained-fact checks.

use std::{future::Future, time::Duration};

use platform::database;
use tokio::time::{Instant, timeout_at};

use crate::runtime;

/// Closed diagnostic identifiers; never include stored or request values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum Stage {
    ClockConnection,
    InstanceLock,
    BarrierConnection,
    DatabasePrerequisites,
    Keys,
    TrustedTime,
    Persons,
    Corrections,
    RevisionSources,
    Cancellations,
    Revisions,
    Accounts,
    Avatars,
    IdentityRelations,
    Capabilities,
    AccessRelations,
    GrantRelations,
    Appointments,
    ChallengesOpen,
    ChallengesBatch,
    ChallengesClose,
    MessagesOpen,
    MessagesBatch,
    MessagesClose,
    AuthenticationsOpen,
    AuthenticationsBatch,
    AuthenticationAccountsOpen,
    AuthenticationAccountsBatch,
    AuthenticationAccountsClose,
    AuthenticationRelations,
    Grants,
    InvitationRelations,
    InvitationsBatch,
    Assignments,
    RegistrationRole,
    RegistrationAttempts,
    RegistrationAttempt,
    RegistrationAttemptReceipt,
    RegistrationReceipts,
    RegistrationReceipt,
    RegistrationSession,
    RegistrationSessionSource,
    Revocations,
    TransfersOpen,
    TransfersBatch,
    TransfersClose,
    TransferRelations,
    TransferCompletionsOpen,
    TransferCompletionsBatch,
    TransferCompletionsClose,
    SuspensionRelations,
    Suspensions,
    Resumptions,
    ResumptionSource,
    Affiliations,
    HistoryStates,
    HistoryRecords,
    Directories,
    BarrierConfirmation,
}

impl Stage {
    /// Observe an existing runtime budget without changing sampling or cleanup semantics.
    pub fn runtime<T>(
        self,
        waiting: Waiting,
        result: Result<T, runtime::Error>,
    ) -> Result<T, runtime::Error> {
        if let Err(mut error) = result.as_ref() {
            while let runtime::Error::Combined { cause, .. } = error {
                error = cause;
            }
            if matches!(error, runtime::Error::Timeout(_))
                || matches!(error, runtime::Error::Connection(cause) if connection_timeout(cause))
            {
                self.report(waiting);
            }
        }
        result
    }

    pub(crate) fn connection(
        self,
        result: Result<database::Session, database::Error>,
    ) -> Result<database::Session, database::Error> {
        if let Err(error) = &result
            && connection_timeout(error)
        {
            self.report(Waiting::Connection);
        }
        result
    }

    pub(crate) async fn connect_at(
        self,
        deadline: Instant,
        future: impl Future<Output = Result<database::Session, database::Error>>,
    ) -> Result<Result<database::Session, database::Error>, Timeout> {
        let result = self.run(deadline, future, Waiting::Connection).await?;
        Ok(self.connection(result))
    }

    pub(crate) async fn query<F: Future>(
        self,
        budget: Duration,
        query: F,
    ) -> Result<F::Output, Timeout> {
        self.run(Instant::now() + budget, query, Waiting::Query)
            .await
    }

    pub(crate) async fn batch<F: Future>(
        self,
        budget: Duration,
        batch: F,
    ) -> Result<F::Output, Timeout> {
        self.batch_at(Instant::now() + budget, batch).await
    }

    pub(crate) async fn batch_at<F: Future>(
        self,
        deadline: Instant,
        batch: F,
    ) -> Result<F::Output, Timeout> {
        self.run(deadline, batch, Waiting::QueryAndValidation).await
    }

    async fn run<F: Future>(
        self,
        deadline: Instant,
        future: F,
        waiting: Waiting,
    ) -> Result<F::Output, Timeout> {
        match timeout_at(deadline, future).await {
            Ok(value) => Ok(value),
            Err(source) => {
                self.report(waiting);
                Err(Timeout {
                    stage: self,
                    waiting,
                    source,
                })
            }
        }
    }

    fn report(self, waiting: Waiting) {
        // Observe only local facts: no extra connection, SQL, or failure-path budget.
        let connection = match waiting {
            Waiting::Connection => "acquiring",
            Waiting::Query | Waiting::QueryAndValidation => "held",
        };
        tracing::warn!(
            target: "domain::startup",
            operation = "startup_verification",
            stage = %self,
            kind = "timeout",
            connection,
            waiting = %waiting,
            "startup verification budget expired",
        );
    }
}

fn connection_timeout(error: &database::Error) -> bool {
    matches!(
        error,
        database::Error::RecoveryWait(_)
            | database::Error::Connection(database::ConnectionError::Timeout(_))
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum Waiting {
    Connection,
    Query,
    QueryAndValidation,
}

/// The original elapsed cause plus the safe context of this verification only.
#[derive(Debug, thiserror::Error)]
#[error("startup verification {stage} budget expired during {waiting}")]
pub struct Timeout {
    stage: Stage,
    waiting: Waiting,
    #[source]
    source: tokio::time::error::Elapsed,
}

impl Timeout {
    pub fn stage(&self) -> Stage {
        self.stage
    }

    pub fn waiting(&self) -> Waiting {
        self.waiting
    }
}
