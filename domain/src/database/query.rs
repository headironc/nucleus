use std::{error, fmt};

use tokio_postgres::error::SqlState;

/// Keep driver classification and decoding diagnostics, never PostgreSQL detail or values.
pub struct Query(pub(crate) tokio_postgres::Error);

impl Query {
    pub fn is_constraint(
        &self,
        code: &SqlState,
        schema: &str,
        table: &str,
        constraint: &str,
    ) -> bool {
        self.0.as_db_error().is_some_and(|error| {
            error.code() == code
                && error.schema() == Some(schema)
                && error.table() == Some(table)
                && error.constraint() == Some(constraint)
        })
    }

    pub fn code(&self) -> Option<&SqlState> {
        self.0.code()
    }
}

impl fmt::Display for Query {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(error) = self.0.as_db_error() {
            write!(formatter, "SQLSTATE {}", error.code().code())
        } else {
            // The driver's Display identifies the column or transport stage, not its cause.
            fmt::Display::fmt(&self.0, formatter)
        }
    }
}

impl fmt::Debug for Query {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl error::Error for Query {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        if self.0.as_db_error().is_some() {
            None
        } else {
            Some(&self.0)
        }
    }
}
