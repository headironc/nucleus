use std::time::Duration;

use platform::{cryptography, database};
use reqwest::StatusCode;
use thiserror::Error;

use crate::{
    database::{Query, transaction::EndError},
    identity::{
        challenge::{client, response},
        current, keys, message,
        mobile::envelope,
        operation, transfer,
    },
    runtime,
};

/// The operation whose fact or time check failed, without original input values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum Stage {
    ChallengeDecode,
    ChallengeValidation,
    ChallengeAdmission,
    ChallengeTransition,
    ChallengeHistory,
    MessageDecode,
    MessageValidation,
    MessagePreparation,
    MessageRecovery,
    MessageTransition,
    MessageClaim,
    MessageConclusion,
    MessageHistory,
    MessageSigning,
    Retention,
    CreateSession,
}

/// Business failure semantics; transports do not interpret transaction wrappers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Proof,
    Denied,
    Absent,
    Mismatch,
    TargetChanged,
    Conflict,
    Rejected,
    Verification,
    CodeInvalid,
    Suspended,
    MobileUnsupported,
    Quota {
        retry_at: chrono::DateTime<chrono::Utc>,
    },
    Unknown,
    Unavailable,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("phone authorization code was explicitly rejected")]
    MobileCodeInvalid,
    #[error("mini-program phone authentication is not configured")]
    MiniUnavailable,
    #[error("trusted phone number is unsupported")]
    MobileUnsupported,
    #[error("mini-program phone exchange failed")]
    Mini {
        #[source]
        cause: crate::identity::mini::client::Error,
        elapsed: Duration,
    },
    #[error("{0}")]
    History(#[from] crate::history::Error),
    #[error("{0}")]
    Current(#[from] current::Error),
    #[error("{0}")]
    Transfer(#[from] transfer::Error),
    #[error("the new phone must differ from the account's current phone")]
    Conflict,
    #[error("the current phone no longer matches the confirmed target")]
    TargetChanged,
    #[error("{0}")]
    Access(#[from] crate::access::Error),
    #[error("the submitted verification code is incorrect")]
    CodeInvalid,
    #[error("the account is suspended")]
    Suspended,
    #[error("{0}")]
    InputKey(#[from] keys::request::Missing),
    #[error("{0}")]
    Credential(#[from] crate::identity::credential::Error),
    #[error("{0}")]
    Supplier(#[from] client::Error),
    #[error("{0}")]
    Response(#[from] response::Error),
    #[error("{cause}")]
    MessageRequest {
        #[source]
        cause: message::request::Error,
        elapsed: Duration,
    },
    #[error("{0}")]
    MessageBuild(#[from] message::request::BuildError),
    #[error("PNVS outcome remains unknown: {reason:?}")]
    MessageOutcome {
        reason: message::response::Unknown,
        status: StatusCode,
        elapsed: Duration,
    },
    #[error("message error with an unconfirmed conclusion")]
    MessageConclusion {
        cause: Box<Error>,
        persistence: Box<Error>,
    },
    #[error("message readback retained the original failure and cleanup results")]
    MessageReadback {
        #[source]
        cause: Box<Error>,
        commit: Result<(), EndError>,
        termination: Result<(), database::Error>,
    },
    #[error("message preparation and the remaining action both failed")]
    MessagePreparation {
        #[source]
        cause: Box<Error>,
        preparation: Box<Error>,
    },
    #[error("message transition cleanup and the remaining action both failed")]
    MessageTransition {
        #[source]
        cause: Box<Error>,
        transition: Box<Error>,
    },
    #[error("current verification or credential handoff is unavailable")]
    Verification,
    #[error("human verification was rejected")]
    Rejected,
    #[error("message quota is exhausted until {retry_at}")]
    Quota {
        retry_at: chrono::DateTime<chrono::Utc>,
    },
    #[error("authentication random material generation failed")]
    Random(#[from] getrandom::Error),
    #[error("{0}")]
    MessageKey(#[from] keys::message::Missing),
    #[error("authentication is closed")]
    Closed,
    #[error("authentication work capacity is exhausted")]
    Busy,
    #[error("original request proof is invalid")]
    Proof,
    #[error("input differs from the original request")]
    Mismatch,
    #[error("challenge token is already occupied")]
    Occupied,
    #[error("authentication fact check failed during {stage}: {rule}")]
    Facts { stage: Stage, rule: &'static str },
    #[error("authentication time check failed during {stage}: {rule}")]
    Time { stage: Stage, rule: &'static str },
    #[error("{0}")]
    Runtime(#[from] runtime::Error),
    #[error("{0}")]
    Database(#[from] crate::database::Error),
    #[error("authentication transaction budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("{0}")]
    VerificationTimeout(#[from] crate::database::verification::Timeout),
    #[error("authentication execution ended unexpectedly")]
    Worker,
    #[error("authentication tasks failed during drain")]
    Drain(Vec<crate::identity::authentication::Task>),
    #[error("{0}")]
    Connection(#[from] database::Error),
    #[error("authentication database query failed: {0}")]
    Query(#[source] Query),
    #[error("authentication session decoding failed: {0}")]
    Decode(#[source] Query),
    #[error("{0}")]
    Cryptography(#[from] cryptography::Error),
    #[error("{0}")]
    Mobile(#[from] envelope::Error),
    #[error("{0}")]
    Key(#[from] keys::challenge::Missing),
    #[error("{0}")]
    Metadata(#[from] crate::keys::database::Error),
    #[error("authentication write did not commit")]
    NotCommitted { cause: Box<Error> },
    #[error("the original retention attempt is complete")]
    RetentionCompleted {
        #[source]
        cause: Box<Error>,
    },
    #[error("retention readback retained the original failure and cleanup results")]
    RetentionReadback {
        #[source]
        cause: Box<Error>,
        commit: Result<(), EndError>,
        termination: Result<(), database::Error>,
    },
    #[error("challenge readback retained the original failure and cleanup results")]
    ChallengeReadback {
        #[source]
        cause: Box<Error>,
        commit: Result<(), EndError>,
        termination: Result<(), database::Error>,
    },
    #[error("challenge finalization and the remaining action both failed")]
    ChallengeConclusion {
        #[source]
        cause: Box<Error>,
        challenge: Box<Error>,
    },
    #[error("the original session cleanup owner is absent")]
    SessionDeleted {
        #[source]
        cause: Box<Error>,
    },
    #[error("session readback retained the original failure and cleanup results")]
    SessionReadback {
        #[source]
        cause: Box<Error>,
        commit: Result<(), EndError>,
        termination: Result<(), database::Error>,
    },
    #[error("session recovery refused delivery after an original failure")]
    SessionRefused {
        #[source]
        cause: Box<Error>,
        refusal: Box<Error>,
    },
    #[error("authentication commit outcome remains unknown")]
    Unknown {
        cause: Box<Error>,
        recovery: Box<Error>,
    },
    #[error("authentication operation and connection termination failed")]
    Combined {
        cause: Box<Error>,
        termination: database::Error,
    },
    #[error("authentication rejection rollback failed")]
    Rollback {
        cause: Box<Error>,
        rollback: EndError,
    },
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::from(Query(error))
    }
}

impl From<Query> for Error {
    fn from(error: Query) -> Self {
        for (table, constraint) in [
            ("challenges", "challenges_transfer_expiry_check"),
            ("confirmations", "confirmations_transfer_expiry_check"),
            ("claims", "claims_transfer_expiry_check"),
        ] {
            if error.is_constraint(
                &tokio_postgres::error::SqlState::CHECK_VIOLATION,
                "identity",
                table,
                constraint,
            ) {
                return Self::Verification;
            }
        }
        Self::Query(error)
    }
}

impl From<crate::identity::session::Error> for Error {
    fn from(error: crate::identity::session::Error) -> Self {
        match error {
            crate::identity::session::Error::History(cause) => Self::History(cause),
            crate::identity::session::Error::Query(cause) => Self::Query(cause),
            crate::identity::session::Error::Decode(cause) => Self::Decode(cause),
            crate::identity::session::Error::Credential(cause) => Self::Credential(cause),
            crate::identity::session::Error::Random(cause) => Self::Random(cause),
            crate::identity::session::Error::Facts { rule } => Self::Facts {
                stage: Stage::CreateSession,
                rule,
            },
            crate::identity::session::Error::Unavailable => Self::Verification,
            crate::identity::session::Error::Time => Self::Facts {
                stage: Stage::CreateSession,
                rule: "session and handoff lifetimes must form timestamps",
            },
        }
    }
}

impl From<crate::database::transaction::Error> for Error {
    fn from(error: crate::database::transaction::Error) -> Self {
        match error {
            crate::database::transaction::Error::Query(cause) => Self::Query(cause),
            crate::database::transaction::Error::Crypto(cause) => Self::Cryptography(cause),
            // The fence already closed admission; the outcome stays with the next process.
            crate::database::transaction::Error::Superseded => Self::Closed,
        }
    }
}

impl From<crate::kernel::tx::BeginError> for Error {
    fn from(error: crate::kernel::tx::BeginError) -> Self {
        match error {
            crate::kernel::tx::BeginError::Coordination(cause) => cause.into(),
            crate::kernel::tx::BeginError::Timeout(cause) => Self::Timeout(cause),
        }
    }
}

impl From<EndError> for Error {
    fn from(error: EndError) -> Self {
        match error {
            EndError::Commit(cause) | EndError::Rollback(cause) => Self::Query(cause),
            EndError::CommitTimeout(cause) | EndError::RollbackTimeout(cause) => {
                Self::Timeout(cause)
            }
        }
    }
}

impl From<runtime::work::Error> for Error {
    fn from(error: runtime::work::Error) -> Self {
        match error {
            runtime::work::Error::Closed => Self::Closed,
            runtime::work::Error::Busy => Self::Busy,
            runtime::work::Error::Worker => Self::Worker,
            runtime::work::Error::Drain(failures) => Self::Drain(failures),
        }
    }
}

impl runtime::work::Failure for Error {
    fn unreliable(&self) -> bool {
        self.unreliable()
    }

    fn report(&self, operation_id: operation::Id) {
        self.report(operation_id);
    }
}

impl Error {
    /// Preserve unknown outcomes and failed termination before exposing a business refusal.
    pub fn failure(&self) -> Failure {
        match self {
            Self::Unknown { .. } | Self::MessageConclusion { .. } => Failure::Unknown,
            Self::NotCommitted { cause }
            | Self::ChallengeReadback { cause, .. }
            | Self::ChallengeConclusion { cause, .. }
            | Self::MessageReadback { cause, .. }
            | Self::MessagePreparation { cause, .. }
            | Self::MessageTransition { cause, .. }
            | Self::SessionReadback { cause, .. } => cause.failure(),
            Self::SessionRefused { refusal, .. } => refusal.failure(),
            Self::Combined { cause, .. } => match cause.failure() {
                Failure::Unknown => Failure::Unknown,
                _ => Failure::Unavailable,
            },
            Self::Proof | Self::MobileCodeInvalid => Failure::Proof,
            Self::Current(current::Error::Invalid) => Failure::Proof,
            Self::CodeInvalid => Failure::CodeInvalid,
            Self::Suspended => Failure::Suspended,
            Self::MobileUnsupported => Failure::MobileUnsupported,
            Self::Mismatch => Failure::Mismatch,
            Self::TargetChanged => Failure::TargetChanged,
            Self::Conflict => Failure::Conflict,
            Self::Transfer(cause) => match cause.failure() {
                transfer::Failure::Invalid => Failure::Proof,
                transfer::Failure::Denied => Failure::Denied,
                transfer::Failure::Absent => Failure::Absent,
                transfer::Failure::Conflict => Failure::Conflict,
                transfer::Failure::Verification => Failure::Verification,
                _ => Failure::Unavailable,
            },
            Self::Occupied | Self::Rejected => Failure::Rejected,
            Self::Verification | Self::Response(response::Error::Expired) => Failure::Verification,
            Self::Quota { retry_at } => Failure::Quota {
                retry_at: *retry_at,
            },
            _ => Failure::Unavailable,
        }
    }

    pub(super) fn report(&self, operation_id: operation::Id) {
        if let Self::Mini { cause, elapsed } = self {
            tracing::error!(%operation_id, stage="mini_phone_exchange", supplier_code=cause.code(), elapsed=?elapsed, error=?cause);
        } else if let Self::NotCommitted { cause } = self {
            if matches!(
                cause.as_ref(),
                Self::SessionReadback { .. }
                    | Self::ChallengeReadback { .. }
                    | Self::MessageReadback { .. }
            ) {
                tracing::error!(%operation_id, stage = "authentication_completion", kind = "unavailable", outcome = "not_committed", error = ?self);
            } else {
                cause.report(operation_id);
            }
        } else if let Self::SessionRefused { cause, refusal } = self {
            if cause.unreliable() || !refusal.rejection() {
                tracing::error!(%operation_id, stage = "authentication_completion", kind = "unavailable", outcome = "known_refusal", error = ?self);
            }
        } else if !self.rejection() && !matches!(self, Self::Response(response::Error::Expired)) {
            let kind = match self.failure() {
                Failure::Unknown => "unknown",
                _ => "unavailable",
            };
            tracing::error!(%operation_id, stage = "authentication_completion", kind, error = ?self);
        }
    }

    pub(super) fn rejection(&self) -> bool {
        if let Self::Transfer(cause) = self {
            return matches!(
                cause.failure(),
                transfer::Failure::Invalid
                    | transfer::Failure::Denied
                    | transfer::Failure::Absent
                    | transfer::Failure::Conflict
                    | transfer::Failure::Verification
            );
        }
        matches!(
            self,
            Self::Proof
                | Self::MobileCodeInvalid
                | Self::MobileUnsupported
                | Self::Current(current::Error::Invalid)
                | Self::TargetChanged
                | Self::Conflict
                | Self::Transfer(transfer::Error::Verification)
                | Self::CodeInvalid
                | Self::Suspended
                | Self::Mismatch
                | Self::Occupied
                | Self::Verification
                | Self::Rejected
                | Self::Quota { .. }
        )
    }

    pub(super) fn unreliable(&self) -> bool {
        match self {
            Self::History(cause) => cause.corrupt(),
            Self::Current(cause) => cause.unreliable(),
            Self::Transfer(cause) => runtime::work::Failure::unreliable(cause),
            Self::Facts { .. }
            | Self::Decode(_)
            | Self::Worker
            | Self::Drain(_)
            | Self::Time { .. }
            | Self::Key(_)
            | Self::MessageKey(_)
            | Self::InputKey(_)
            | Self::Credential(_)
            | Self::Mobile(_) => true,
            Self::Runtime(error) => error.unreliable(),
            Self::Query(query) => std::error::Error::source(&query.0)
                .is_some_and(|cause| cause.is::<cryptography::envelope::ParseError>()),
            Self::RetentionCompleted { cause }
            | Self::RetentionReadback { cause, .. }
            | Self::SessionDeleted { cause }
            | Self::NotCommitted { cause }
            | Self::ChallengeReadback { cause, .. }
            | Self::MessageReadback { cause, .. }
            | Self::SessionReadback { cause, .. }
            | Self::Rollback { cause, .. } => cause.unreliable(),
            Self::MessageConclusion { cause, persistence } => {
                cause.unreliable() || persistence.unreliable()
            }
            Self::ChallengeConclusion { cause, challenge } => {
                cause.unreliable() || challenge.unreliable()
            }
            Self::MessagePreparation { cause, preparation } => {
                cause.unreliable() || preparation.unreliable()
            }
            Self::MessageTransition { cause, transition } => {
                cause.unreliable() || transition.unreliable()
            }
            Self::Unknown { cause, recovery } => cause.unreliable() || recovery.unreliable(),
            Self::SessionRefused { cause, refusal } => cause.unreliable() || refusal.unreliable(),
            Self::Combined { cause, .. } => cause.unreliable(),
            _ => false,
        }
    }

    pub(super) fn combine(self, termination: Result<(), database::Error>) -> Self {
        match termination {
            Ok(()) => self,
            Err(error) => Self::Combined {
                cause: Box::new(self),
                termination: error,
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use tokio::{runtime::Runtime, time::timeout};

    use crate::identity::authentication::{
        Error, Failure,
        verification::tests::{Database, server},
    };

    #[test]
    fn database_refusals_require_the_exact_transfer_expiry_constraint() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(std::time::Duration::from_secs(30), async {
                let database = Database::new(server).await;
                let reader = database.connections(&database.user).recover().await.expect("isolated runtime identity");
                for (fields, refused) in [
                    ("ERRCODE='23514', SCHEMA='identity', TABLE='challenges', CONSTRAINT='challenges_transfer_expiry_check'", true),
                    ("ERRCODE='23514', SCHEMA='identity', TABLE='confirmations', CONSTRAINT='confirmations_transfer_expiry_check'", true),
                    ("ERRCODE='23514', SCHEMA='identity', TABLE='claims', CONSTRAINT='claims_transfer_expiry_check'", true),
                    ("ERRCODE='23503', SCHEMA='identity', TABLE='challenges', CONSTRAINT='challenges_transfer_expiry_check'", false),
                    ("ERRCODE='23514', SCHEMA='public', TABLE='challenges', CONSTRAINT='challenges_transfer_expiry_check'", false),
                    ("ERRCODE='23514', SCHEMA='identity', TABLE='messages', CONSTRAINT='challenges_transfer_expiry_check'", false),
                    ("ERRCODE='23514', SCHEMA='identity', TABLE='challenges', CONSTRAINT='claims_transfer_expiry_check'", false),
                    ("ERRCODE='23514', TABLE='challenges', CONSTRAINT='challenges_transfer_expiry_check'", false),
                    ("ERRCODE='23514', SCHEMA='identity', CONSTRAINT='challenges_transfer_expiry_check'", false),
                    ("ERRCODE='23514', SCHEMA='identity', TABLE='challenges'", false),
                ] {
                    let cause = reader.client().batch_execute(&format!("DO $$ BEGIN RAISE EXCEPTION 'fictional private detail' USING {fields}; END $$")).await.expect_err("structured PostgreSQL error");
                    let error = Error::from(cause);
                    assert_eq!(error.rejection(), refused, "{fields}");
                    assert_eq!(error.failure(), if refused { Failure::Verification } else { Failure::Unavailable }, "{fields}");
                    if !refused { assert!(matches!(error, Error::Query(_)), "technical cause remains available"); }
                    assert!(!format!("{error:?}").contains("fictional private detail"));
                }
                reader.terminate().await.expect("reader ended");
            }).await.expect("bounded error classification");
        });
    }

    #[test]
    fn completed_retention_keeps_an_unreliable_original_failure() {
        for complete in [true, false] {
            let cause = Box::new(Error::RetentionReadback {
                cause: Box::new(Error::Time {
                    stage: crate::identity::authentication::Stage::Retention,
                    rule: "fictional unreliable original observation",
                }),
                commit: Ok(()),
                termination: Ok(()),
            });
            let error = if complete {
                Error::RetentionCompleted { cause }
            } else {
                Error::NotCommitted { cause }
            };
            assert!(
                error.unreliable(),
                "a known result cannot erase the original fatal classification"
            );
            assert_eq!(error.failure(), Failure::Unavailable);
        }
    }

    #[tokio::test]
    async fn transaction_wrappers_preserve_failure_semantics_without_discarding_causes() {
        let retry_at = chrono::DateTime::from_timestamp(1_800_000_000, 0).expect("fixture time");
        for (cause, expected) in [
            (Error::Proof, Failure::Proof),
            (Error::Mismatch, Failure::Mismatch),
            (Error::Rejected, Failure::Rejected),
            (Error::Occupied, Failure::Rejected),
            (Error::Verification, Failure::Verification),
            (
                Error::Response(crate::identity::challenge::response::Error::Expired),
                Failure::Verification,
            ),
            (Error::Quota { retry_at }, Failure::Quota { retry_at }),
            (Error::Closed, Failure::Unavailable),
            (
                Error::Unknown {
                    cause: Box::new(Error::Busy),
                    recovery: Box::new(Error::Closed),
                },
                Failure::Unknown,
            ),
            (
                Error::MessageConclusion {
                    cause: Box::new(Error::Busy),
                    persistence: Box::new(Error::Closed),
                },
                Failure::Unknown,
            ),
        ] {
            assert_eq!(cause.failure(), expected);
            let error = Error::NotCommitted {
                cause: Box::new(Error::NotCommitted {
                    cause: Box::new(cause),
                }),
            };
            assert_eq!(error.failure(), expected);
            let allowance = tokio::sync::Semaphore::new(0);
            allowance.close();
            let termination = platform::database::Error::RecoveryClosed(
                allowance
                    .acquire()
                    .await
                    .expect_err("closed fixture allowance"),
            );
            let error = Error::Combined {
                cause: Box::new(error),
                termination,
            };
            let expected = if expected == Failure::Unknown {
                Failure::Unknown
            } else {
                Failure::Unavailable
            };
            assert_eq!(error.failure(), expected);
            assert!(matches!(error, Error::Combined { .. }));
        }
    }

    #[test]
    fn message_preparation_diagnostics_keep_later_classification_and_fatal_causes() {
        for (cause, expected) in [
            (
                Error::MessageOutcome {
                    reason: crate::identity::message::response::Unknown::HttpStatus,
                    status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
                    elapsed: std::time::Duration::from_millis(7),
                },
                Failure::Unavailable,
            ),
            (Error::Verification, Failure::Verification),
            (
                Error::Unknown {
                    cause: Box::new(Error::Busy),
                    recovery: Box::new(Error::Closed),
                },
                Failure::Unknown,
            ),
        ] {
            // Constructed diagnostic evidence checks propagation, not a real corrupt-storage recovery.
            let error = Error::ChallengeConclusion {
                cause: Box::new(Error::MessagePreparation {
                    cause: Box::new(cause),
                    preparation: Box::new(Error::MessageReadback {
                        cause: Box::new(Error::Facts {
                            stage: super::Stage::MessageRecovery,
                            rule: "fixture original fact failure",
                        }),
                        commit: Ok(()),
                        termination: Ok(()),
                    }),
                }),
                challenge: Box::new(Error::Busy),
            };
            assert_eq!(error.failure(), expected);
            assert!(error.unreliable());
        }
    }

    #[test]
    fn session_readback_wrappers_preserve_refusal_and_unreliable_original_causes() {
        for (refusal, expected) in [
            (Error::Proof, Failure::Proof),
            (Error::Mismatch, Failure::Mismatch),
            (Error::Verification, Failure::Verification),
        ] {
            let error = Error::SessionRefused {
                cause: Box::new(Error::Facts {
                    stage: super::Stage::CreateSession,
                    rule: "original session source",
                }),
                refusal: Box::new(refusal),
            };
            assert_eq!(error.failure(), expected);
            assert!(
                error.unreliable(),
                "a known refusal cannot hide the original integrity failure"
            );
        }
        let error = Error::NotCommitted {
            cause: Box::new(Error::SessionReadback {
                cause: Box::new(Error::Facts {
                    stage: super::Stage::CreateSession,
                    rule: "original session source",
                }),
                commit: Ok(()),
                termination: Ok(()),
            }),
        };
        assert_eq!(error.failure(), Failure::Unavailable);
        assert!(
            error.unreliable(),
            "known absence cannot hide the original integrity failure"
        );
    }
}
