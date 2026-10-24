//! Assisted establishment combines current management qualification and Identity's original fact.

#[cfg(test)]
mod tests;

use std::{sync::Arc, time::Duration};

use serde::Deserialize;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Client;
use tracing::Instrument;
use utoipa::ToSchema;

use crate::{
    account::Service,
    database::transaction::{self, Completion, EndError, Known, Outcome},
    identity::{
        account,
        current::{self, Actor},
        mobile::Mobile,
        operation, person, transfer,
    },
    runtime::{registry::Family, work},
};

const BUDGET: Duration = Duration::from_secs(10);

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = transfer::Assistance)]
pub struct Input {
    person_id: person::Id,
    revision_id: person::revision::Id,
    #[schema(value_type = String, pattern = "^(?:13[0-9]|14[01456879]|15[0-35-9]|16[2567]|17[0-8]|18[0-9]|19[0-35-9])[0-9]{8}$")]
    mobile: Mobile,
    /// Must be true: the manager explicitly confirms having verified this person.
    verified: transfer::Verified,
}

impl Input {
    fn proof(
        &self,
        actor: &Actor,
        operation_id: operation::Id,
        operator_id: person::Id,
    ) -> transfer::Assistance<'_> {
        transfer::Assistance {
            operation_id,
            operator_id,
            person_id: self.person_id,
            revision_id: self.revision_id,
            session_id: actor.session_id(),
            mobile: &self.mobile,
            verified: &self.verified,
        }
    }
}

impl Service {
    pub async fn create_transfer(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        account_id: account::Id,
        input: Input,
    ) -> Result<transfer::Transfer, Error> {
        let service = self.clone();
        self.work
            .run(
                Arc::clone(&self.registry),
                Family::Transfer,
                operation_id,
                async move {
                    service
                        .establish_transfer(&actor, operation_id, account_id, &input)
                        .await
                },
            )
            .instrument(tracing::info_span!("assisted_transfer"))
            .await
    }

    async fn write_transfer(
        &self,
        client: &Client,
        actor: &Actor,
        operation_id: operation::Id,
        account_id: account::Id,
        input: &Input,
        attempted: &mut bool,
    ) -> Result<Outcome<transfer::Transfer>, Error> {
        transaction::begin(&self.context, client, transfer::FAMILY, operation_id).await?;
        let option = transfer::find_by_operation(client, self.roles.keys(), operation_id).await?;
        let target = transfer::Target::assisted(
            client,
            &self.roles,
            &self.current,
            &self.keys,
            actor,
            account_id,
        )
        .await?;
        let now = target.observed_at();
        let proof = input.proof(actor, operation_id, target.operator_id());
        if let Some(original) = option {
            if target.person_id() != input.person_id {
                return Err(transfer::Error::Mismatch.into());
            }
            return Ok(Outcome::Known(
                original.deliver_assisted(&self.keys, &proof, now)?,
            ));
        }
        if target.person_id() != input.person_id
            || target.revision_id() != input.revision_id
            || target.mobile().as_ref() != input.mobile.as_ref()
        {
            return Err(Error::TargetChanged);
        }
        *attempted = true;
        Ok(Outcome::Pending(
            transfer::Original::establish(
                client,
                &self.keys,
                &proof,
                target.operator_revision_id(),
                now,
            )
            .await?,
        ))
    }

    async fn establish_transfer(
        &self,
        actor: &Actor,
        operation_id: operation::Id,
        account_id: account::Id,
        input: &Input,
    ) -> Result<transfer::Transfer, Error> {
        let lease = self.context.connections().lease().await?;
        let deadline = Instant::now() + BUDGET;
        let mut attempted = false;
        let result = timeout_at(
            deadline,
            self.write_transfer(
                lease.client(),
                actor,
                operation_id,
                account_id,
                input,
                &mut attempted,
            ),
        )
        .await;
        let cause = match result {
            Ok(Ok(outcome)) => {
                let (completion, cleanup) =
                    transaction::commit(outcome, deadline, lease.client().batch_execute("COMMIT"))
                        .await
                        .recycle(lease)
                        .await;
                match completion {
                    Completion::Known(Known { value, end }) => {
                        if end.is_err() || cleanup.is_err() {
                            tracing::error!(%operation_id, stage = "assisted_transfer_completion", outcome = "known", commit = ?end, cleanup = ?cleanup);
                        }
                        return Ok(value);
                    }
                    Completion::Pending { error, .. } => Error::End(error).combine(cleanup),
                }
            }
            Ok(Err(cause)) if cause.rejected() => {
                let (known, cleanup) = transaction::rollback(
                    cause,
                    deadline,
                    lease.client().batch_execute("ROLLBACK"),
                )
                .await
                .recycle(lease)
                .await;
                return Err(match known.end {
                    Ok(()) => known.value,
                    Err(rollback) => Error::Rollback {
                        cause: Box::new(known.value),
                        rollback,
                    },
                }
                .combine(cleanup));
            }
            Ok(Err(cause)) => cause.combine(lease.discard().await),
            Err(cause) => Error::from(cause).combine(lease.discard().await),
        };
        if !attempted {
            return Err(cause);
        }
        match self.recover_transfer(actor, operation_id, input).await {
            Ok((
                Known {
                    value: Some(value),
                    end,
                },
                termination,
            )) => {
                let unreliable = work::Failure::unreliable(&cause);
                if unreliable {
                    self.registry.close();
                }
                if unreliable || end.is_err() || termination.is_err() {
                    tracing::error!(%operation_id, stage = "assisted_transfer_completion", outcome = "committed", error = ?cause, commit = ?end, termination = ?termination);
                }
                Ok(value)
            }
            Ok((
                Known {
                    value: None,
                    end: commit,
                },
                termination,
            )) => Err(Error::NotCommitted {
                cause: Box::new(cause),
                commit,
                termination,
            }),
            Err(recovery) => Err(Error::Unknown {
                cause: Box::new(cause),
                recovery: Box::new(recovery),
            }),
        }
    }

    async fn recover_transfer(
        &self,
        actor: &Actor,
        operation_id: operation::Id,
        input: &Input,
    ) -> Result<
        (
            Known<Option<transfer::Transfer>>,
            Result<(), platform::database::Error>,
        ),
        Error,
    > {
        let session = self.context.connections().recover().await?;
        let deadline = Instant::now() + BUDGET;
        let result = async {
            let option = timeout_at(deadline, async {
                transaction::begin(
                    &self.context,
                    session.client(),
                    transfer::FAMILY,
                    operation_id,
                )
                .await?;
                let option =
                    transfer::find_by_operation(session.client(), self.roles.keys(), operation_id)
                        .await?;
                let now = self.current.now(session.client()).await?;
                option
                    .map(|original| {
                        let proof = input.proof(actor, operation_id, original.operator_id());
                        original.deliver_assisted(&self.keys, &proof, now)
                    })
                    .transpose()
                    .map_err(Error::from)
            })
            .await??;
            Ok::<_, Error>(
                transaction::confirm(option, deadline, session.client().batch_execute("COMMIT"))
                    .await,
            )
        }
        .await;
        match result {
            Ok(result) => Ok(result.terminate(session).await),
            Err(error) => Err(error.combine(session.terminate().await)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the confirmed target has changed")]
    TargetChanged,
    #[error("{0}")]
    Current(#[from] current::Error),
    #[error("{0}")]
    Transfer(#[from] transfer::Error),
    #[error("assisted transfer query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("assisted transfer lock encoding failed")]
    Cryptography(#[from] platform::cryptography::Error),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("{0}")]
    Work(#[from] work::Error),
    #[error("assisted transfer budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("assisted transfer transaction end failed: {0}")]
    End(#[source] EndError),
    #[error("assisted transfer did not commit")]
    NotCommitted {
        cause: Box<Error>,
        commit: Result<(), EndError>,
        termination: Result<(), platform::database::Error>,
    },
    #[error("assisted transfer outcome remains unknown")]
    Unknown {
        cause: Box<Error>,
        recovery: Box<Error>,
    },
    #[error("assisted transfer refusal rollback failed")]
    Rollback {
        cause: Box<Error>,
        rollback: EndError,
    },
    #[error("assisted transfer and connection termination failed")]
    Combined {
        cause: Box<Error>,
        termination: platform::database::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Invalid,
    Denied,
    Absent,
    TargetChanged,
    Mismatch,
    Unknown,
    Unavailable,
}

impl From<transaction::Error> for Error {
    fn from(error: transaction::Error) -> Self {
        match error {
            transaction::Error::Query(cause) => Self::Query(cause),
            transaction::Error::Crypto(cause) => Self::Cryptography(cause),
            // The fence already closed admission; the outcome stays with the next process.
            transaction::Error::Superseded => Self::Work(work::Error::Closed),
        }
    }
}

impl Error {
    pub fn failure(&self) -> Failure {
        match self {
            Self::Current(current::Error::Invalid) => Failure::Invalid,
            Self::TargetChanged => Failure::TargetChanged,
            Self::Transfer(cause) => match cause.failure() {
                transfer::Failure::Invalid => Failure::Invalid,
                transfer::Failure::Denied => Failure::Denied,
                transfer::Failure::Absent => Failure::Absent,
                transfer::Failure::Mismatch => Failure::Mismatch,
                _ => Failure::Unavailable,
            },
            Self::Unknown { .. } => Failure::Unknown,
            Self::Combined { cause, .. } if cause.failure() == Failure::Unknown => Failure::Unknown,
            _ => Failure::Unavailable,
        }
    }

    fn rejected(&self) -> bool {
        matches!(
            self.failure(),
            Failure::Invalid
                | Failure::Denied
                | Failure::Absent
                | Failure::TargetChanged
                | Failure::Mismatch
        )
    }

    fn combine(self, ended: Result<(), platform::database::Error>) -> Self {
        match ended {
            Ok(()) => self,
            Err(termination) => Self::Combined {
                cause: Box::new(self),
                termination,
            },
        }
    }
}

impl work::Failure for Error {
    fn unreliable(&self) -> bool {
        match self {
            Self::Current(error) => error.unreliable(),
            Self::Transfer(error) => error.unreliable(),
            Self::NotCommitted { cause, .. }
            | Self::Rollback { cause, .. }
            | Self::Combined { cause, .. } => cause.unreliable(),
            Self::Unknown { cause, recovery } => cause.unreliable() || recovery.unreliable(),
            _ => false,
        }
    }

    fn report(&self, operation_id: operation::Id) {
        if !self.rejected() {
            tracing::error!(%operation_id, stage = "assisted_transfer_completion", kind = ?self.failure(), error = ?self);
        }
    }
}
