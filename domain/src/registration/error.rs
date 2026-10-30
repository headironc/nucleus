use chrono::{DateTime, Utc};
use platform::database;

use crate::{
    access::role,
    database::{Query, transaction::EndError},
    identity::{account, credential, keys, mobile, operation, person},
    runtime,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Proof,
    Mismatch,
    Verification,
    InvalidDate,
    MobileTaken,
    IdNumberTaken,
    Quota { retry_at: DateTime<Utc> },
    Unknown,
    Unavailable,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    History(#[from] crate::history::Error),
    #[error("original registration proof is invalid")]
    Proof,
    #[error("input differs from the original registration")]
    Mismatch,
    #[error("registration credential or original session handoff is unavailable")]
    Verification,
    #[error("the mobile is already assigned to an account")]
    MobileTaken,
    #[error("the identity number is already assigned to an account")]
    IdNumberTaken,
    #[error("registration admission quota is exhausted until {retry_at}")]
    Quota { retry_at: DateTime<Utc> },
    #[error("{0}")]
    Date(#[from] person::number::ParseError),
    #[error("registration fact check failed: {rule}")]
    Facts { rule: &'static str },
    #[error("registration transaction budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("{0}")]
    VerificationTimeout(#[from] crate::database::verification::Timeout),
    #[error("registration database query failed: {0}")]
    Query(#[from] Query),
    #[error("registration session decoding failed: {0}")]
    Decode(#[source] Query),
    #[error("{0}")]
    Runtime(#[from] runtime::Error),
    #[error("{0}")]
    Work(#[from] runtime::work::Error),
    #[error("{0}")]
    Connection(#[from] database::Error),
    #[error("{0}")]
    Credential(#[from] credential::Error),
    #[error("{0}")]
    Mobile(#[from] mobile::envelope::Error),
    #[error("{0}")]
    Revision(#[from] person::revision::Error),
    #[error("{0}")]
    Account(#[from] account::Error),
    #[error("{0}")]
    Role(#[from] role::Error),
    #[error("{0}")]
    Key(#[from] keys::request::Missing),
    #[error("registration cryptographic operation failed")]
    Crypto(#[from] platform::cryptography::Error),
    #[error("registration credential generation failed")]
    Random(#[from] getrandom::Error),
    #[error("registration write did not commit")]
    NotCommitted { cause: Box<Error> },
    #[error("the original registration cleanup entities are absent")]
    Deleted {
        #[source]
        cause: Box<Error>,
    },
    #[error("registration readback retained the original failure and cleanup results")]
    Readback {
        #[source]
        cause: Box<Error>,
        commit: Result<(), EndError>,
        termination: Result<(), database::Error>,
    },
    #[error("registration commit outcome remains unknown")]
    Unknown {
        cause: Box<Error>,
        recovery: Box<Error>,
    },
    #[error("registration rejection rollback failed")]
    Rollback {
        cause: Box<Error>,
        rollback: EndError,
    },
    #[error("registration operation and connection termination failed")]
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

impl From<crate::database::transaction::Error> for Error {
    fn from(error: crate::database::transaction::Error) -> Self {
        match error {
            crate::database::transaction::Error::Query(cause) => Self::Query(cause),
            crate::database::transaction::Error::Crypto(cause) => Self::Crypto(cause),
            // The fence already closed admission; the outcome stays with the next process.
            crate::database::transaction::Error::Superseded => {
                Self::Work(runtime::work::Error::Closed)
            }
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

impl From<crate::identity::session::Error> for Error {
    fn from(error: crate::identity::session::Error) -> Self {
        match error {
            crate::identity::session::Error::History(cause) => Self::History(cause),
            crate::identity::session::Error::Query(cause) => Self::Query(cause),
            crate::identity::session::Error::Decode(cause) => Self::Decode(cause),
            crate::identity::session::Error::Credential(cause) => Self::Credential(cause),
            crate::identity::session::Error::Random(cause) => Self::Random(cause),
            crate::identity::session::Error::Facts { rule } => Self::Facts { rule },
            crate::identity::session::Error::Unavailable => Self::Verification,
            crate::identity::session::Error::Time => Self::Facts {
                rule: "session and handoff lifetimes must form timestamps",
            },
        }
    }
}

impl Error {
    pub fn failure(&self) -> Failure {
        match self {
            Self::Proof => Failure::Proof,
            Self::Mismatch => Failure::Mismatch,
            Self::Verification => Failure::Verification,
            Self::Date(_) => Failure::InvalidDate,
            Self::MobileTaken => Failure::MobileTaken,
            Self::IdNumberTaken => Failure::IdNumberTaken,
            Self::Quota { retry_at } => Failure::Quota {
                retry_at: *retry_at,
            },
            Self::Unknown { .. } => Failure::Unknown,
            Self::NotCommitted { cause } | Self::Readback { cause, .. } => cause.failure(),
            Self::Combined { cause, .. } if cause.failure() == Failure::Unknown => Failure::Unknown,
            _ => Failure::Unavailable,
        }
    }

    pub(super) fn rejected(&self) -> bool {
        matches!(
            self,
            Self::Proof | Self::Mismatch | Self::Verification | Self::Date(_) | Self::Quota { .. }
        )
    }

    pub(super) fn combine(self, termination: Result<(), database::Error>) -> Self {
        match termination {
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
            Self::History(cause) => cause.corrupt(),
            Self::Revision(error) => error.unreliable(),
            Self::Facts { .. }
            | Self::Decode(_)
            | Self::Credential(_)
            | Self::Mobile(_)
            | Self::Key(_) => true,
            Self::Runtime(error) => error.unreliable(),
            Self::Query(error) | Self::Account(account::Error::Database(error)) => {
                std::error::Error::source(&error.0).is_some_and(|source| {
                    source.is::<platform::cryptography::envelope::ParseError>()
                })
            }
            Self::Account(_) => true,
            Self::Role(error) => runtime::work::Failure::unreliable(error),
            Self::Deleted { cause }
            | Self::NotCommitted { cause }
            | Self::Readback { cause, .. }
            | Self::Combined { cause, .. }
            | Self::Rollback { cause, .. } => cause.unreliable(),
            Self::Unknown { cause, recovery } => cause.unreliable() || recovery.unreliable(),
            _ => false,
        }
    }

    fn report(&self, operation_id: operation::Id) {
        if let Self::NotCommitted { cause } = self {
            if matches!(cause.as_ref(), Self::Readback { .. }) {
                tracing::error!(%operation_id, stage = "registration_completion", kind = "unavailable", outcome = "not_committed", error = ?self);
            } else {
                cause.report(operation_id);
            }
        } else if !self.rejected() && !matches!(self, Self::MobileTaken | Self::IdNumberTaken) {
            let kind = match self.failure() {
                Failure::Unknown => "unknown",
                _ => "unavailable",
            };
            tracing::error!(%operation_id, stage = "registration_completion", kind, error = ?self);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use tracing_test::traced_test;

    use crate::{registration::Error, runtime::work::Failure};

    #[test]
    #[traced_test]
    fn logs_technical_completion_without_turning_business_refusals_into_failures() {
        let operation_id = "00000000-0000-4000-8000-000000000001"
            .parse()
            .expect("operation");
        for error in [
            Error::Proof,
            Error::Mismatch,
            Error::Verification,
            Error::MobileTaken,
            Error::IdNumberTaken,
            Error::Date(crate::identity::person::number::ParseError::Future),
            Error::NotCommitted {
                cause: Box::new(Error::Proof),
            },
        ] {
            error.report(operation_id);
        }
        assert!(!logs_contain("registration_completion"));
        Error::Facts {
            rule: "original session source",
        }
        .report(operation_id);
        assert!(logs_contain("registration_completion"));
        assert!(logs_contain("original session source"));
        assert!(logs_contain("00000000-0000-4000-8000-000000000001"));
    }
}
