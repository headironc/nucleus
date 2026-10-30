use platform::database;
use thiserror::Error;

use crate::database::Query;

#[derive(Debug, Error)]
pub enum Error {
    #[error("trusted database time is closed after shutdown or an unreliable runtime observation")]
    ClockClosed,
    #[error("database time must not precede the process or persisted high-water mark")]
    ClockInvalid,
    #[error("another application or deployment command holds the database instance lock")]
    InstanceHeld,
    #[error("the instance session no longer holds the database instance lock")]
    InstanceLost,
    #[error("a later process superseded the generation this command advanced")]
    InstanceSuperseded,
    #[error("runtime budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("runtime maintenance task ended unexpectedly")]
    Worker(crate::runtime::work::Task),
    #[error("{0}")]
    Connection(#[from] database::Error),
    #[error("runtime database query failed: {0}")]
    Query(#[from] Query),
    #[error("runtime operation and connection termination failed")]
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
    pub(crate) fn unreliable(&self) -> bool {
        match self {
            Self::ClockClosed
            | Self::ClockInvalid
            | Self::InstanceHeld
            | Self::InstanceLost
            | Self::InstanceSuperseded
            | Self::Worker(_) => true,
            Self::Combined { cause, .. } => cause.unreliable(),
            Self::Timeout(_) | Self::Connection(_) | Self::Query(_) => false,
        }
    }

    pub(crate) fn combine(self, termination: Result<(), database::Error>) -> Self {
        match termination {
            Ok(()) => self,
            Err(error) => Self::Combined {
                cause: Box::new(self),
                termination: error,
            },
        }
    }
}
