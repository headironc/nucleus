use platform::cryptography::{self, Encoded, sha256::Digest};
use tokio_postgres::Client;

use crate::identity::{authentication::Error, challenge::Token, operation};

pub(super) async fn begin(client: &Client, operation_id: operation::Id) -> Result<(), Error> {
    client.batch_execute("BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL statement_timeout = '10s'; SET LOCAL idle_in_transaction_session_timeout = '10s'").await?;
    client
        .batch_execute("SELECT pg_catalog.pg_advisory_xact_lock_shared(1314210636, 1)")
        .await?;
    client
        .query_one(
            "SELECT pg_catalog.pg_advisory_xact_lock($1)",
            &[&key(b"challenge/operation", operation_id.as_bytes())?],
        )
        .await?;
    Ok(())
}

pub(super) async fn token(client: &Client, token: &Token) -> Result<(), Error> {
    object(client, b"challenge/token", token.as_bytes()).await
}

pub(super) async fn object(client: &Client, domain: &[u8], value: &[u8]) -> Result<(), Error> {
    client
        .query_one(
            "SELECT pg_catalog.pg_advisory_xact_lock($1)",
            &[&key(domain, value)?],
        )
        .await?;
    Ok(())
}

fn key(domain: &[u8], value: &[u8]) -> Result<i64, cryptography::Error> {
    struct Input<'a> {
        domain: &'a [u8],
        value: &'a [u8],
    }
    impl cryptography::Input<2> for Input<'_> {
        fn fields(&self) -> [&[u8]; 2] {
            [self.domain, self.value]
        }
    }
    let digest = Digest::new(&Encoded::new(&Input { domain, value })?);
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&digest.as_ref()[..8]);
    Ok(i64::from_be_bytes(bytes))
}
