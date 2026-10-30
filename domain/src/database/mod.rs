//! Project database prerequisites shared by deployment and application startup.

pub mod instance;
pub mod permissions;
mod query;
pub(crate) mod read;
pub(crate) mod sealed;
pub mod transaction;
pub mod verification;

use std::time::Duration;

use platform::database::{self, Session, User};
use thiserror::Error;
use tokio::time::Instant;
use tokio_postgres::Client;

pub use crate::database::query::Query;
use crate::runtime::Context;

/// Holds the exclusive database barrier and the exact runtime whose permissions were checked.
pub struct Barrier {
    context: Context,
    session: Session,
}

impl Barrier {
    pub async fn begin(context: Context) -> Result<Self, Error> {
        let session = verification::Stage::BarrierConnection
            .connection(context.connections().recover().await)?;
        Self::verify(context, session, Instant::now() + Duration::from_secs(10)).await
    }

    /// Share an enclosing startup deadline so its cancellation cannot hide the active check.
    pub async fn begin_at(context: Context, deadline: Instant) -> Result<Self, Error> {
        let session = verification::Stage::BarrierConnection
            .connect_at(deadline, context.connections().recover())
            .await??;
        Self::verify(
            context,
            session,
            deadline.min(Instant::now() + Duration::from_secs(10)),
        )
        .await
    }

    async fn verify(context: Context, session: Session, deadline: Instant) -> Result<Self, Error> {
        let error = match verification::Stage::DatabasePrerequisites
            .batch_at(deadline, async {
                let client = session.client();
                client
                    .batch_execute(
                        "BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL statement_timeout = '10s'",
                    )
                    .await?;
                client
                    .batch_execute("SELECT pg_catalog.pg_advisory_xact_lock(1314210636, 1)")
                    .await?;
                let user: User = client
                    .query_one("SELECT current_user::text", &[])
                    .await?
                    .try_get(0)?;
                permissions::verify(client, &user).await?;
                // Every transaction of an earlier generation holds the shared barrier that
                // this exclusive barrier has outwaited; afterwards their process is fenced.
                Ok::<_, Error>(instance::advance(client).await?)
            })
            .await
        {
            Ok(Ok(generation)) => {
                context.fence(generation);
                return Ok(Self { context, session });
            }
            Ok(Err(error)) => error,
            Err(error) => error.into(),
        };
        Err(error.combine(session.terminate().await))
    }

    pub(crate) fn context(&self) -> &Context {
        &self.context
    }
    pub(crate) fn client(&self) -> &Client {
        self.session.client()
    }

    pub(crate) async fn confirm(&self) -> Result<(), Error> {
        self.session.client().batch_execute("COMMIT").await?;
        Ok(())
    }

    pub(crate) async fn terminate(self) -> Result<(), database::Error> {
        self.session.terminate().await
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("runtime database permissions are invalid")]
    Permissions,
    #[error("database prerequisite query failed: {0}")]
    Query(#[from] Query),
    #[error("{0}")]
    Connection(#[from] database::Error),
    #[error("{0}")]
    Timeout(#[from] crate::database::verification::Timeout),
    #[error("database prerequisite and connection termination failed")]
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
    fn combine(self, termination: Result<(), database::Error>) -> Self {
        match termination {
            Ok(()) => self,
            Err(error) => Self::Combined {
                cause: Box::new(self),
                termination: error,
            },
        }
    }
}
