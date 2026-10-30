//! One error for every command and query: a closed business rejection or one technical fault.

use std::{convert::Infallible, fmt};

use crate::{
    access::{appointment, assignment, grant, role},
    database::Query,
    identity::{account, current, person, person::revision},
    runtime::{self, work},
};

/// The public class of a business rejection; HTTP chooses the exact status from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Forbidden,
    NotFound,
    Conflict,
    Gone,
    Unprocessable,
}

/// A command's or query's closed business rejection with its own status and application code.
pub trait Rejection: fmt::Debug + Send + 'static {
    fn status(&self) -> Status;
    fn code(&self) -> &'static str;
}

impl Rejection for Infallible {
    fn status(&self) -> Status {
        match *self {}
    }

    fn code(&self) -> &'static str {
        match *self {}
    }
}

#[derive(Debug)]
pub enum Error<R> {
    /// A determined business rejection.
    Rejected(R),
    /// The subject does not hold.
    Unauthenticated,
    /// The subject lacks the action or object permission.
    Denied,
    /// The original operation has a different request.
    Mismatch,
    /// Work capacity is exhausted.
    Busy,
    /// A dependency or a required fact is unavailable.
    Unavailable(Fault),
    /// The commit outcome is not yet confirmed.
    Unknown(Fault),
    /// Stored facts are damaged; the executor closes admission.
    Corrupt(Fault),
}

impl<R> Error<R> {
    pub(crate) fn changed(&self) -> bool {
        self.fault().is_some_and(|fault| {
            matches!(
                fault.kind(),
                FaultKind::History(crate::history::Error::Changed)
            )
        })
    }
    pub(crate) fn query(error: tokio_postgres::Error) -> Self {
        Self::Unavailable(Fault::new(Stage::Command, FaultKind::Query(Query(error))))
    }

    pub(crate) fn decode(error: tokio_postgres::Error) -> Self {
        Self::Corrupt(Fault::new(Stage::Command, FaultKind::Decode(Query(error))))
    }

    pub(crate) fn connection(error: platform::database::Error) -> Self {
        Self::Unavailable(Fault::new(Stage::Connection, FaultKind::Connection(error)))
    }

    /// A violated integrity rule found while reading stored facts.
    pub(crate) fn corrupt(rule: &'static str) -> Self {
        Self::Corrupt(Fault::new(Stage::Command, FaultKind::Integrity(rule)))
    }

    pub(crate) fn lock_set_changed() -> Self {
        Self::Unavailable(Fault::new(Stage::Lock, FaultKind::LockSetChanged))
    }

    pub(crate) fn changed_locks(&self) -> bool {
        matches!(self, Self::Unavailable(fault) if matches!(fault.kind(), FaultKind::LockSetChanged))
    }

    pub(crate) fn elapsed<T>(elapsed: tokio::time::error::Elapsed) -> Result<T, Self> {
        Err(Self::Unavailable(Fault::new(
            Stage::Command,
            FaultKind::Timeout(elapsed),
        )))
    }

    pub(crate) fn clock(error: runtime::Error) -> Self {
        Self::Unavailable(Fault::new(Stage::Time, FaultKind::Clock(error)))
    }

    /// Assign a fault raised inside one executor step to that step.
    pub(crate) fn at(self, stage: Stage) -> Self {
        match self {
            Self::Unavailable(fault) => Self::Unavailable(fault.at(stage)),
            Self::Unknown(fault) => Self::Unknown(fault.at(stage)),
            Self::Corrupt(fault) => Self::Corrupt(fault.at(stage)),
            other => other,
        }
    }

    pub fn fault(&self) -> Option<&Fault> {
        match self {
            Self::Unavailable(fault) | Self::Unknown(fault) | Self::Corrupt(fault) => Some(fault),
            _ => None,
        }
    }

    /// Admission was closed before or during this work.
    pub fn is_closed(&self) -> bool {
        self.fault()
            .is_some_and(|fault| matches!(fault.kind(), FaultKind::Closed))
    }

    pub fn is_corrupt(&self) -> bool {
        matches!(self, Self::Corrupt(_))
    }

    /// A determined result that writes nothing and needs no recovery.
    pub(crate) fn is_rejection(&self) -> bool {
        matches!(
            self,
            Self::Rejected(_) | Self::Unauthenticated | Self::Denied | Self::Mismatch | Self::Busy
        )
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Rejected(_) => "rejected",
            Self::Unauthenticated => "unauthenticated",
            Self::Denied => "denied",
            Self::Mismatch => "mismatch",
            Self::Busy => "busy",
            Self::Unavailable(_) => "unavailable",
            Self::Unknown(_) => "unknown",
            Self::Corrupt(_) => "corrupt",
        }
    }
}

impl<R> crate::database::read::Failure for Error<R> {
    fn changed(&self) -> bool {
        self.changed()
    }
}

impl<R: Rejection> Error<R> {
    /// The application code of a determined rejection; technical failures have none.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::Rejected(rejection) => Some(rejection.code()),
            Self::Unauthenticated => Some("authentication_invalid"),
            Self::Denied => Some("permission_denied"),
            Self::Mismatch => Some("request_mismatch"),
            Self::Busy => Some("rate_limited"),
            Self::Unavailable(_) | Self::Unknown(_) | Self::Corrupt(_) => None,
        }
    }
}

impl<R> fmt::Display for Error<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(_) => formatter.write_str("the request was rejected"),
            Self::Unauthenticated => formatter.write_str("the subject does not hold"),
            Self::Denied => formatter.write_str("the subject lacks the permission"),
            Self::Mismatch => formatter.write_str("the original operation has a different request"),
            Self::Busy => formatter.write_str("work capacity is exhausted"),
            Self::Unavailable(fault) => write!(formatter, "unavailable at {:?}", fault.stage()),
            Self::Unknown(fault) => write!(formatter, "outcome unknown at {:?}", fault.stage()),
            Self::Corrupt(fault) => {
                write!(formatter, "stored facts damaged at {:?}", fault.stage())
            }
        }
    }
}

impl<R: fmt::Debug> std::error::Error for Error<R> {}

impl<R> From<Query> for Error<R> {
    fn from(error: Query) -> Self {
        Self::Unavailable(Fault::new(Stage::Command, FaultKind::Query(error)))
    }
}

impl<R> From<work::Error> for Error<R> {
    fn from(error: work::Error) -> Self {
        match error {
            work::Error::Closed => {
                Self::Unavailable(Fault::new(Stage::Admission, FaultKind::Closed))
            }
            work::Error::Busy => Self::Busy,
            // Accepted work ended without a result; whether it committed is not known.
            work::Error::Worker | work::Error::Drain(_) => {
                Self::Unknown(Fault::new(Stage::Execution, FaultKind::Worker))
            }
        }
    }
}

impl<R: Rejection> work::Failure for Error<R> {
    fn unreliable(&self) -> bool {
        self.is_corrupt()
    }

    fn report(&self, operation_id: crate::identity::operation::Id) {
        if !self.is_rejection() {
            tracing::error!(%operation_id, stage = "command", kind = self.label(), error = ?self);
        }
    }
}

/// The current session check classifies its own failures.
impl<R> From<current::Error> for Error<R> {
    fn from(error: current::Error) -> Self {
        match error {
            current::Error::Invalid => Self::Unauthenticated,
            current::Error::History(error) => error.into(),
            current::Error::Runtime(error) => Self::clock(error),
            current::Error::Work(work::Error::Closed) => {
                Self::Unavailable(Fault::new(Stage::Admission, FaultKind::Closed))
            }
            error if error.unreliable() => {
                Self::Corrupt(Fault::new(Stage::Command, FaultKind::Identity(error)))
            }
            error => Self::Unavailable(Fault::new(Stage::Command, FaultKind::Identity(error))),
        }
    }
}

/// The current membership check classifies its own failures.
impl<R> From<grant::Error> for Error<R> {
    fn from(error: grant::Error) -> Self {
        match error {
            grant::Error::Current(error) => error.into(),
            grant::Error::Role(error) => error.into(),
            grant::Error::Denied => Self::Denied,
            error if work::Failure::unreliable(&error) => {
                Self::Corrupt(Fault::new(Stage::Command, FaultKind::Access(error)))
            }
            error => Self::Unavailable(Fault::new(Stage::Command, FaultKind::Access(error))),
        }
    }
}

impl<R> From<role::Error> for Error<R> {
    fn from(error: role::Error) -> Self {
        match error {
            role::Error::Denied => Self::Denied,
            role::Error::Current(error) => error.into(),
            role::Error::History(error) => error.into(),
            error => {
                let corrupt = work::Failure::unreliable(&error);
                let fault = Fault::new(Stage::Command, FaultKind::Role(error));
                if corrupt {
                    Self::Corrupt(fault)
                } else {
                    Self::Unavailable(fault)
                }
            }
        }
    }
}

impl<R> From<appointment::Error> for Error<R> {
    fn from(error: appointment::Error) -> Self {
        let error = match error {
            appointment::Error::Role(error) => return error.into(),
            appointment::Error::Current(error) => return error.into(),
            error => error,
        };
        let corrupt = work::Failure::unreliable(&error);
        let fault = Fault::new(Stage::Command, FaultKind::Appointment(error));
        if corrupt {
            Self::Corrupt(fault)
        } else {
            Self::Unavailable(fault)
        }
    }
}

impl<R> From<assignment::Error> for Error<R> {
    fn from(error: assignment::Error) -> Self {
        match error {
            assignment::Error::Denied => Self::Denied,
            assignment::Error::Current(error) => error.into(),
            assignment::Error::Role(error) => error.into(),
            error => {
                let corrupt = work::Failure::unreliable(&error);
                let fault = Fault::new(Stage::Command, FaultKind::Assignment(error));
                if corrupt {
                    Self::Corrupt(fault)
                } else {
                    Self::Unavailable(fault)
                }
            }
        }
    }
}

impl<R> From<account::ReadError> for Error<R> {
    fn from(error: account::ReadError) -> Self {
        let error = match error {
            account::ReadError::History(error) => return error.into(),
            error => error,
        };
        let corrupt = error.unreliable();
        let fault = Fault::new(Stage::Command, FaultKind::Account(error));
        if corrupt {
            Self::Corrupt(fault)
        } else {
            Self::Unavailable(fault)
        }
    }
}

impl<R> From<account::Error> for Error<R> {
    fn from(error: account::Error) -> Self {
        Self::Unavailable(Fault::new(
            Stage::Command,
            FaultKind::AccountCreation(error),
        ))
    }
}

impl<R> From<crate::identity::suspension::Error> for Error<R> {
    fn from(error: crate::identity::suspension::Error) -> Self {
        let error = match error {
            crate::identity::suspension::Error::History(error) => return error.into(),
            error => error,
        };
        let corrupt = error.unreliable();
        let fault = Fault::new(Stage::Command, FaultKind::Suspension(error));
        if corrupt {
            Self::Corrupt(fault)
        } else {
            Self::Unavailable(fault)
        }
    }
}

impl<R> From<person::ReadError> for Error<R> {
    fn from(error: person::ReadError) -> Self {
        let error = match error {
            person::ReadError::History(error) => return error.into(),
            person::ReadError::Account(error) => return error.into(),
            error => error,
        };
        let corrupt = error.unreliable();
        let fault = Fault::new(Stage::Command, FaultKind::Person(error));
        if corrupt {
            Self::Corrupt(fault)
        } else {
            Self::Unavailable(fault)
        }
    }
}

impl<R> From<revision::ReadError> for Error<R> {
    fn from(error: revision::ReadError) -> Self {
        let corrupt = error.unreliable();
        let fault = Fault::new(Stage::Command, FaultKind::Revision(error));
        if corrupt {
            Self::Corrupt(fault)
        } else {
            Self::Unavailable(fault)
        }
    }
}

impl<R> From<crate::identity::correction::Error> for Error<R> {
    fn from(error: crate::identity::correction::Error) -> Self {
        let corrupt = error.unreliable();
        let fault = Fault::new(Stage::Command, FaultKind::Correction(error));
        if corrupt {
            Self::Corrupt(fault)
        } else {
            Self::Unavailable(fault)
        }
    }
}

impl<R> From<crate::identity::cancellation::Error> for Error<R> {
    fn from(error: crate::identity::cancellation::Error) -> Self {
        let corrupt = error.unreliable();
        let fault = Fault::new(Stage::Command, FaultKind::Cancellation(error));
        if corrupt {
            Self::Corrupt(fault)
        } else {
            Self::Unavailable(fault)
        }
    }
}

/// The executor step at which a fault arose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Admission,
    Encoding,
    Connection,
    Begin,
    Lock,
    Time,
    Authorize,
    Completion,
    Load,
    Write,
    Replay,
    Commit,
    Rollback,
    Termination,
    Recovery,
    Read,
    Retention,
    Execution,
    /// Raised by a command's own step before the executor assigns that step.
    Command,
}

/// One technical failure: its stage, typed cause and every later finalization failure.
pub struct Fault(Box<Inner>);

#[derive(Debug)]
struct Inner {
    stage: Stage,
    kind: FaultKind,
    later: Vec<(Stage, FaultKind)>,
}

impl Fault {
    pub(crate) fn new(stage: Stage, kind: FaultKind) -> Self {
        Self(Box::new(Inner {
            stage,
            kind,
            later: Vec::new(),
        }))
    }

    pub fn stage(&self) -> Stage {
        self.0.stage
    }

    pub fn kind(&self) -> &FaultKind {
        &self.0.kind
    }

    /// Finalization, rollback or recovery failures that followed the primary cause.
    pub fn later(&self) -> impl Iterator<Item = (Stage, &FaultKind)> {
        self.0.later.iter().map(|(stage, kind)| (*stage, kind))
    }

    pub(crate) fn then(mut self, later: Fault) -> Self {
        let Inner {
            stage,
            kind,
            later: mut rest,
        } = *later.0;
        self.0.later.push((stage, kind));
        self.0.later.append(&mut rest);
        self
    }

    /// Appends a later failure; a later determined result adds no technical cause.
    pub(crate) fn then_error<R>(self, later: Error<R>) -> Self {
        match later {
            Error::Unavailable(fault) | Error::Unknown(fault) | Error::Corrupt(fault) => {
                self.then(fault)
            }
            _ => self,
        }
    }

    pub(crate) fn then_result(self, later: Result<(), Fault>) -> Self {
        match later {
            Ok(()) => self,
            Err(later) => self.then(later),
        }
    }

    fn at(mut self, stage: Stage) -> Self {
        if self.0.stage == Stage::Command {
            self.0.stage = stage;
        }
        self
    }
}

impl fmt::Debug for Fault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, formatter)
    }
}

/// Closed typed causes. Each keeps its real type and never a payload or secret.
#[derive(Debug)]
pub enum FaultKind {
    History(crate::history::Error),
    Role(role::Error),
    Appointment(appointment::Error),
    Assignment(assignment::Error),
    Account(account::ReadError),
    AccountCreation(account::Error),
    Suspension(crate::identity::suspension::Error),
    Person(person::ReadError),
    Revision(revision::ReadError),
    Correction(crate::identity::correction::Error),
    Cancellation(crate::identity::cancellation::Error),
    Query(Query),
    Decode(Query),
    Connection(platform::database::Error),
    Timeout(tokio::time::error::Elapsed),
    Crypto(platform::cryptography::Error),
    Encryption(crate::kernel::keys::Error),
    File(platform::files::Error),
    Avatar(crate::identity::avatar::Error),
    Encoding(serde_json::Error),
    Integrity(&'static str),
    Clock(runtime::Error),
    Closed,
    Superseded,
    Worker,
    LockSetChanged,
    /// Technical mobile workflows still own their code proofs and qualification errors.
    Transfer(crate::identity::transfer::Error),
    /// Transitional: the session check stays with Identity until the executor resolves subjects.
    Identity(current::Error),
    /// Transitional: the membership check stays with Access until it returns kernel errors.
    Access(grant::Error),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::convert::Infallible;

    use super::{Error, Fault, FaultKind, Stage};
    use crate::{access::grant, identity::current, runtime};

    #[test]
    fn identity_clock_failures_are_unavailable_and_fact_damage_is_corrupt() {
        tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(async {
                let elapsed =
                    tokio::time::timeout(std::time::Duration::ZERO, std::future::pending::<()>())
                        .await
                        .expect_err("actual clock budget expiration");
                for clock in [
                    runtime::Error::ClockClosed,
                    runtime::Error::Timeout(elapsed),
                ] {
                    let error = Error::<Infallible>::from(current::Error::Runtime(clock));
                    let Error::Unavailable(fault) = error else {
                        panic!("a failed clock is not damaged identity data: {error:?}");
                    };
                    assert_eq!(fault.stage(), Stage::Time);
                    assert!(matches!(fault.kind(), FaultKind::Clock(_)));
                }
                let error = Error::<Infallible>::from(grant::Error::Current(
                    current::Error::Runtime(runtime::Error::ClockClosed),
                ));
                assert!(matches!(error, Error::Unavailable(_)));
                let error = Error::<Infallible>::from(current::Error::Facts {
                    rule: "the actor retains its original account",
                });
                assert!(matches!(error, Error::Corrupt(_)));
            });
    }

    #[test]
    fn errors_stay_small_and_later_faults_keep_their_order() {
        assert!(std::mem::size_of::<Error<Infallible>>() <= 16);
        let fault = Fault::new(Stage::Commit, FaultKind::Worker)
            .then(Fault::new(Stage::Termination, FaultKind::Closed))
            .then(
                Fault::new(Stage::Recovery, FaultKind::Superseded)
                    .then(Fault::new(Stage::Termination, FaultKind::Integrity("rule"))),
            );
        let later: Vec<_> = fault.later().map(|(stage, _)| stage).collect();
        assert_eq!(
            later,
            [Stage::Termination, Stage::Recovery, Stage::Termination]
        );
        let error = Error::<Infallible>::Unavailable(fault).at(Stage::Load);
        assert_eq!(error.fault().expect("fault").stage(), Stage::Commit);
        let staged = Error::<Infallible>::corrupt("rule").at(Stage::Load);
        assert_eq!(staged.fault().expect("fault").stage(), Stage::Load);
        assert!(staged.is_corrupt());
    }
}
