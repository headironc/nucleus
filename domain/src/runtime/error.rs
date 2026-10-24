use platform::database;
use thiserror::Error;

use crate::database::Query;

#[derive(Debug, Error)]
pub enum Error {
    #[error(
        "trusted database time sampling is closed after termination or an earlier failed attempt"
    )]
    ClockClosed,
    #[error(
        "trusted database time requires exactly one clock row with observed_at not later than the new sample"
    )]
    ClockInvalid,
    #[error("runtime budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
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
