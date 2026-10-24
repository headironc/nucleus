use platform::{cryptography, database};
use thiserror::Error;

use crate::{
    database::Query,
    identity::{
        challenge::{client, mobile, response},
        keys, message, operation,
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
}

/// Business failure semantics; transports do not interpret transaction wrappers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Proof,
    Mismatch,
    Rejected,
    Verification,
    Quota {
        retry_at: chrono::DateTime<chrono::Utc>,
    },
    Unknown,
    Unavailable,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Supplier(#[from] client::Error),
    #[error("{0}")]
    Response(#[from] response::Error),
    #[error("{0}")]
    MessageRequest(#[from] message::request::Error),
    #[error("{0}")]
    MessageBuild(#[from] message::request::BuildError),
    #[error("PNVS outcome remains unknown: {0:?}")]
    MessageOutcome(message::response::Unknown),
    #[error("message error with an unconfirmed conclusion")]
    MessageConclusion {
        cause: Box<Error>,
        persistence: Box<Error>,
    },
    #[error("message permit requires a current passed challenge")]
    Verification,
    #[error("human verification was rejected")]
    Rejected,
    #[error("message quota is exhausted until {retry_at}")]
    Quota {
        retry_at: chrono::DateTime<chrono::Utc>,
    },
    #[error("code generation failed")]
    Random(#[from] getrandom::Error),
    #[error("{0}")]
    MessageKey(#[from] keys::message::Missing),
    #[error("authentication is closed")]
    Closed,
    #[error("authentication work capacity is exhausted")]
    Busy,
    #[error("original challenge proof is invalid")]
    Proof,
    #[error("challenge input differs from the original request")]
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
    #[error("authentication execution ended unexpectedly")]
    Worker,
    #[error("authentication tasks failed during drain")]
    Drain(Vec<crate::identity::authentication::Task>),
    #[error("{0}")]
    Connection(#[from] database::Error),
    #[error("authentication database query failed: {0}")]
    Query(#[from] Query),
    #[error("{0}")]
    Cryptography(#[from] cryptography::Error),
    #[error("{0}")]
    Mobile(#[from] mobile::Error),
    #[error("{0}")]
    Key(#[from] keys::challenge::Missing),
    #[error("{0}")]
    Metadata(#[from] keys::database::Error),
    #[error("authentication write did not commit")]
    NotCommitted { cause: Box<Error> },
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
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(Query(error))
    }
}

impl Error {
    /// Preserve unknown outcomes and failed termination before exposing a business refusal.
    pub fn failure(&self) -> Failure {
        match self {
            Self::Unknown { .. } | Self::MessageConclusion { .. } => Failure::Unknown,
            Self::NotCommitted { cause } => cause.failure(),
            Self::Combined { cause, .. } => match cause.failure() {
                Failure::Unknown => Failure::Unknown,
                _ => Failure::Unavailable,
            },
            Self::Proof => Failure::Proof,
            Self::Mismatch => Failure::Mismatch,
            Self::Occupied | Self::Rejected => Failure::Rejected,
            Self::Verification | Self::Response(response::Error::Expired) => Failure::Verification,
            Self::Quota { retry_at } => Failure::Quota {
                retry_at: *retry_at,
            },
            _ => Failure::Unavailable,
        }
    }

    pub(super) fn report(&self, operation_id: operation::Id) {
        if let Self::NotCommitted { cause } = self {
            cause.report(operation_id);
        } else if !self.rejection() && !matches!(self, Self::Response(response::Error::Expired)) {
            let kind = match self.failure() {
                Failure::Unknown => "unknown",
                _ => "unavailable",
            };
            tracing::error!(%operation_id, stage = "authentication_completion", kind, error = ?self);
        }
    }

    pub(super) fn rejection(&self) -> bool {
        matches!(
            self,
            Self::Proof
                | Self::Mismatch
                | Self::Occupied
                | Self::Verification
                | Self::Rejected
                | Self::Quota { .. }
        )
    }

    pub(super) fn unreliable(&self) -> bool {
        match self {
            Self::Facts { .. }
            | Self::Worker
            | Self::Drain(_)
            | Self::Time { .. }
            | Self::Key(_)
            | Self::MessageKey(_)
            | Self::Mobile(_)
            | Self::Runtime(_) => true,
            Self::NotCommitted { cause } => cause.unreliable(),
            Self::MessageConclusion { cause, persistence } => {
                cause.unreliable() || persistence.unreliable()
            }
            Self::Unknown { cause, recovery } => cause.unreliable() || recovery.unreliable(),
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
    use crate::identity::authentication::{Error, Failure};

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
}
