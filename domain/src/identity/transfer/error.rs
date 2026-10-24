use crate::{
    database::{Query, transaction::EndError},
    identity::{current, keys, mobile::envelope, operation},
    runtime,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Invalid,
    Denied,
    Absent,
    Mismatch,
    CodeInvalid,
    Verification,
    Conflict,
    Taken,
    Unknown,
    Unavailable,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Revision(#[from] crate::identity::person::revision::Error),
    #[error("the target is unavailable")]
    Absent,
    #[error("input differs from the original mobile transfer")]
    Mismatch,
    #[error("the submitted verification code is incorrect")]
    CodeInvalid,
    #[error("a current mobile-change verification is required")]
    Verification,
    #[error("the new phone must differ from the current phone")]
    Conflict,
    #[error("the new phone belongs to another current account")]
    Taken,
    #[error("account removal prevents classifying an absent mobile completion")]
    Evidence,
    #[error("mobile transfer fact check failed: {rule}")]
    Facts { rule: &'static str },
    #[error("{0}")]
    History(#[from] crate::history::Error),
    #[error("{0}")]
    Account(#[from] crate::identity::account::ReadError),
    #[error("{0}")]
    Completion(Box<super::completion::Error>),
    #[error("{0}")]
    Current(#[from] current::Error),
    #[error("{0}")]
    Role(#[from] crate::access::role::Error),
    #[error("{0}")]
    Appointment(#[from] crate::access::appointment::Error),
    #[error("mobile transfer query failed: {0}")]
    Query(#[source] Query),
    #[error("{0}")]
    Cryptography(#[from] platform::cryptography::Error),
    #[error("{0}")]
    InputKey(#[from] keys::request::Missing),
    #[error("{0}")]
    MessageKey(#[from] keys::message::Missing),
    #[error("{0}")]
    Mobile(#[from] envelope::Error),
    #[error("{0}")]
    Runtime(#[from] runtime::Error),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("{0}")]
    Work(#[from] runtime::work::Error),
    #[error("mobile transfer budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("{0}")]
    VerificationTimeout(#[from] crate::database::verification::Timeout),
    #[error("mobile transfer did not commit")]
    NotCommitted {
        cause: Box<Error>,
        commit: Result<(), EndError>,
        termination: Result<(), platform::database::Error>,
    },
    #[error("original transfer retention completed with diagnostics")]
    Deleted {
        cause: Box<Error>,
        commit: Result<(), EndError>,
        termination: Result<(), platform::database::Error>,
    },
    #[error("mobile transfer outcome remains unknown")]
    Unknown {
        cause: Box<Error>,
        recovery: Box<Error>,
    },
    #[error("mobile transfer refusal rollback failed")]
    Rollback {
        cause: Box<Error>,
        rollback: EndError,
    },
    #[error("mobile transfer operation and connection termination failed")]
    Combined {
        cause: Box<Error>,
        termination: platform::database::Error,
    },
    #[error("mobile transfer transaction end failed: {0}")]
    End(#[from] EndError),
}

impl From<super::completion::Error> for Error {
    fn from(error: super::completion::Error) -> Self {
        Self::Completion(Box::new(error))
    }
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::from(Query::from(error))
    }
}

impl From<Query> for Error {
    fn from(error: Query) -> Self {
        use tokio_postgres::error::SqlState;

        if error.is_constraint(
            &SqlState::UNIQUE_VIOLATION,
            "identity",
            "accounts",
            "accounts_mobile_hash_key",
        ) {
            return Self::Taken;
        }
        for (table, expiry, unchanged) in [
            (
                "completions",
                "completions_completion_expiry_check",
                "completions_mobile_change_check",
            ),
            (
                "rejections",
                "rejections_completion_expiry_check",
                "rejections_mobile_change_check",
            ),
        ] {
            if error.is_constraint(&SqlState::CHECK_VIOLATION, "identity", table, expiry) {
                return Self::Verification;
            }
            if error.is_constraint(&SqlState::CHECK_VIOLATION, "identity", table, unchanged) {
                return Self::Conflict;
            }
        }
        Self::Query(error)
    }
}

impl From<crate::database::transaction::Error> for Error {
    fn from(error: crate::database::transaction::Error) -> Self {
        match error {
            crate::database::transaction::Error::Query(cause) => Self::from(cause),
            crate::database::transaction::Error::Crypto(cause) => Self::Cryptography(cause),
            // The fence already closed admission; the outcome stays with the next process.
            crate::database::transaction::Error::Superseded => {
                Self::Work(runtime::work::Error::Closed)
            }
        }
    }
}

impl Error {
    pub(super) fn digest(_: std::array::TryFromSliceError) -> Self {
        Self::Facts {
            rule: "original input digest contains exactly 32 bytes",
        }
    }

    pub fn failure(&self) -> Failure {
        match self {
            Self::Completion(error) => match error.as_ref() {
                crate::kernel::Error::Unauthenticated => Failure::Invalid,
                crate::kernel::Error::Denied => Failure::Denied,
                crate::kernel::Error::Mismatch => Failure::Mismatch,
                crate::kernel::Error::Unknown(_) => Failure::Unknown,
                crate::kernel::Error::Rejected(rejection) => match rejection {
                    super::completion::Rejection::Absent => Failure::Absent,
                    super::completion::Rejection::CodeInvalid => Failure::CodeInvalid,
                    super::completion::Rejection::Verification => Failure::Verification,
                    super::completion::Rejection::Conflict => Failure::Conflict,
                    super::completion::Rejection::Taken => Failure::Taken,
                },
                _ => Failure::Unavailable,
            },
            Self::Current(current::Error::Invalid)
            | Self::Role(crate::access::role::Error::Current(current::Error::Invalid)) => {
                Failure::Invalid
            }
            Self::Role(crate::access::role::Error::Denied) => Failure::Denied,
            Self::Absent => Failure::Absent,
            Self::Mismatch => Failure::Mismatch,
            Self::CodeInvalid => Failure::CodeInvalid,
            Self::Verification => Failure::Verification,
            Self::Conflict => Failure::Conflict,
            Self::Taken => Failure::Taken,
            Self::Unknown { .. } => Failure::Unknown,
            Self::Combined { cause, .. } if cause.failure() == Failure::Unknown => Failure::Unknown,
            _ => Failure::Unavailable,
        }
    }

    pub(super) fn rejected(&self) -> bool {
        matches!(
            self.failure(),
            Failure::Invalid
                | Failure::Denied
                | Failure::Absent
                | Failure::Mismatch
                | Failure::CodeInvalid
                | Failure::Verification
                | Failure::Conflict
                | Failure::Taken
        )
    }

    pub(super) fn combine(self, ended: Result<(), platform::database::Error>) -> Self {
        match ended {
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
            Self::History(error) => error.corrupt(),
            Self::Revision(error) => error.unreliable(),
            Self::Account(error) => error.unreliable(),
            Self::Completion(error) => error.is_corrupt(),
            Self::Facts { .. } | Self::Mobile(_) => true,
            Self::Current(error) => error.unreliable(),
            Self::Role(error) => error.unreliable(),
            Self::Appointment(error) => error.unreliable(),
            Self::Runtime(error) => error.unreliable(),
            Self::NotCommitted { cause, .. }
            | Self::Deleted { cause, .. }
            | Self::Combined { cause, .. }
            | Self::Rollback { cause, .. } => cause.unreliable(),
            Self::Unknown { cause, recovery } => cause.unreliable() || recovery.unreliable(),
            _ => false,
        }
    }

    fn report(&self, operation_id: operation::Id) {
        if !self.rejected() {
            tracing::error!(%operation_id, stage = "transfer_completion", kind = ?self.failure(), error = ?self);
        }
    }
}
