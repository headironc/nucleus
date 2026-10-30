use platform::database::Lease;
use tokio::time::Instant;
use tokio_postgres::Client;

use crate::{
    identity::{
        authentication::{Error, TRANSACTION_BUDGET},
        challenge::Token,
        operation,
    },
    runtime::Context,
};

/// These refusals occur before COMMIT; failed cleanup cannot make them committed outcomes.
pub(super) async fn rollback(lease: Lease, cause: Error) -> Error {
    let (completion, cleanup) = crate::database::transaction::rollback(
        cause,
        Instant::now() + TRANSACTION_BUDGET,
        lease.client().batch_execute("ROLLBACK"),
    )
    .await
    .recycle(lease)
    .await;
    refusal(completion).combine(cleanup)
}

pub(super) fn refusal(completion: crate::database::transaction::Known<Error>) -> Error {
    match completion.end {
        Ok(()) => completion.value,
        Err(rollback) => Error::Rollback {
            cause: Box::new(completion.value),
            rollback,
        },
    }
}

pub(super) async fn begin(
    context: &Context,
    client: &Client,
    operation_id: operation::Id,
) -> Result<(), Error> {
    begin_for(context, client, b"challenge/operation", operation_id).await
}

pub(super) async fn begin_for(
    context: &Context,
    client: &Client,
    family: &[u8],
    operation_id: operation::Id,
) -> Result<(), Error> {
    Ok(crate::database::transaction::begin(context, client, family, operation_id).await?)
}

pub(super) async fn token(client: &Client, token: &Token) -> Result<(), Error> {
    object(client, b"challenge/token", token.as_bytes()).await
}

pub(super) async fn object(client: &Client, domain: &[u8], value: &[u8]) -> Result<(), Error> {
    Ok(crate::database::transaction::object(client, domain, value).await?)
}
