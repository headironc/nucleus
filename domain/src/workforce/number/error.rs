//! Number-specific business refusals; transaction failures belong to the kernel.

use crate::kernel::{self, Status};

pub type Error = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Year,
    Absent,
    NumberAbsent,
    TargetChanged,
    Unchanged,
    Recovery,
    StateConflict,
    Conflict,
    Occupied,
    Exhausted,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> Status {
        match self {
            Self::Year | Self::Unchanged | Self::Recovery => Status::Unprocessable,
            Self::Absent => Status::NotFound,
            Self::NumberAbsent
            | Self::TargetChanged
            | Self::StateConflict
            | Self::Conflict
            | Self::Occupied
            | Self::Exhausted => Status::Conflict,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Year | Self::Unchanged | Self::Recovery => "invalid_input",
            Self::Absent | Self::NumberAbsent => "target_unavailable",
            Self::TargetChanged => "target_changed",
            Self::StateConflict => "state_conflict",
            Self::Conflict => "number_present",
            Self::Occupied => "number_occupied",
            Self::Exhausted => "number_pool_exhausted",
        }
    }
}

impl From<Rejection> for Error {
    fn from(value: Rejection) -> Self {
        Self::Rejected(value)
    }
}
