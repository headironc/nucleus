//! Key registration and comparison inside caller-owned transactions.

use std::fmt;

use platform::cryptography::sha256::Digest;
use thiserror::Error;
use tokio_postgres::{GenericClient, Transaction};

use crate::{
    database::transaction::Outcome,
    keys::{Id, metadata::Metadata},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Empty,
    Matching,
}

/// Compare one database snapshot. This neither writes nor establishes readiness.
pub async fn verify(metadata: &Metadata, client: &impl GenericClient) -> Result<(), Error> {
    match inspect(metadata, client).await? {
        State::Matching => Ok(()),
        State::Empty => Err(Error::Empty),
    }
}

/// Populate only an entirely empty registry; matching retries are read-only.
/// The caller owns commit and must recover uncertain outcomes before reporting success.
pub async fn initialize(
    metadata: &Metadata,
    transaction: &Transaction<'_>,
) -> Result<Outcome<()>, Error> {
    if inspect_locked(metadata, transaction).await? == State::Matching {
        return Ok(Outcome::Known(()));
    }
    for (id, fingerprint, active) in metadata.versions() {
        transaction
            .execute(
                r#"INSERT INTO security.keys (id, fingerprint, active) VALUES ($1, $2, $3)"#,
                &[&id, fingerprint, &active],
            )
            .await?;
    }
    Ok(Outcome::Pending(()))
}

/// Serialize initialization and outcome readback using the same transaction lock.
/// Callers must use READ COMMITTED so the following statement gets a fresh snapshot.
pub async fn inspect_locked(
    metadata: &Metadata,
    transaction: &Transaction<'_>,
) -> Result<State, Error> {
    let isolation: String = transaction
        .query_one(r#"SHOW transaction_isolation"#, &[])
        .await?
        .try_get(0)?;
    if isolation != "read committed" {
        return Err(Error::Isolation);
    }
    transaction
        .batch_execute(r#"SELECT pg_catalog.pg_advisory_xact_lock(1314210636, 1)"#)
        .await?;
    inspect(metadata, transaction).await
}

async fn inspect(metadata: &Metadata, client: &impl GenericClient) -> Result<State, Error> {
    let rows = client
        .query(r#"SELECT id, fingerprint, active FROM security.keys"#, &[])
        .await?;
    if rows.is_empty() {
        return Ok(State::Empty);
    }
    if rows.len() != metadata.len() {
        return Err(Error::Mismatch);
    }
    for row in rows {
        let id: Id = row.try_get("id")?;
        let Some((expected, active)) = metadata.find(id) else {
            return Err(Error::Mismatch);
        };
        let fingerprint: Digest = row.try_get("fingerprint")?;
        if &fingerprint != expected || row.try_get::<_, bool>("active")? != active {
            return Err(Error::Mismatch);
        }
    }
    Ok(State::Matching)
}

#[derive(Error)]
pub enum Error {
    #[error("key initialization requires READ COMMITTED")]
    Isolation,
    #[error("key metadata is not initialized")]
    Empty,
    #[error("key metadata does not match the loaded root versions")]
    Mismatch,
    #[error("key metadata database operation failed")]
    Database(#[from] tokio_postgres::Error),
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}
