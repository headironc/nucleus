//! Identity key registration and comparison inside caller-owned transactions.

use std::fmt;

use platform::cryptography::sha256::Digest;
use thiserror::Error;
use tokio_postgres::{GenericClient, Transaction};

use crate::identity::keys::{Id, Purpose, metadata::Metadata};

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
pub async fn initialize(metadata: &Metadata, transaction: &Transaction<'_>) -> Result<(), Error> {
    if inspect_locked(metadata, transaction).await? == State::Matching {
        return Ok(());
    }
    for ((purpose, id), version) in metadata.versions() {
        let sql = match purpose {
            Purpose::Challenge => {
                "INSERT INTO security.challenges (id, fingerprint, active) VALUES ($1, $2, $3)"
            }
            Purpose::Mobile => {
                "INSERT INTO security.mobiles (id, fingerprint, active) VALUES ($1, $2, $3)"
            }
            Purpose::Encryption => {
                "INSERT INTO security.encryptions (id, fingerprint, active) VALUES ($1, $2, $3)"
            }
            Purpose::Message => {
                "INSERT INTO security.messages (id, fingerprint, active) VALUES ($1, $2, $3)"
            }
        };
        transaction
            .execute(sql, &[id, &version.fingerprint, &version.active])
            .await?;
    }
    Ok(())
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
        .query(
            r#"SELECT 'challenge' AS purpose, id, fingerprint, active FROM security.challenges
         UNION ALL SELECT 'mobile', id, fingerprint, active FROM security.mobiles
         UNION ALL SELECT 'encryption', id, fingerprint, active FROM security.encryptions
         UNION ALL SELECT 'message', id, fingerprint, active FROM security.messages"#,
            &[],
        )
        .await?;
    if rows.is_empty() {
        return Ok(State::Empty);
    }
    if rows.len() != metadata.versions().len() {
        return Err(Error::Mismatch);
    }
    for row in rows {
        let purpose: Purpose = row.try_get("purpose")?;
        let id: Id = row.try_get("id")?;
        let Some(version) = metadata.versions().get(&(purpose, id)) else {
            return Err(Error::Mismatch);
        };
        let fingerprint: Digest = row.try_get("fingerprint")?;
        if fingerprint != version.fingerprint || row.try_get::<_, bool>("active")? != version.active
        {
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
    #[error("key metadata does not match loaded key sets")]
    Mismatch,
    #[error("key metadata database operation failed")]
    Database(#[from] tokio_postgres::Error),
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}
