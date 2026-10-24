//! Message transactions and recovery; fixed queries and facts belong to authentication::database.

use chrono::TimeDelta;
use tokio::time::timeout;
use tokio_postgres::Client;

use crate::{
    identity::{
        authentication::{
            self, Error, Inner, Input, Stage, Status, TRANSACTION_BUDGET,
            database::{self, message::Message},
            dispatch::{Change, Preparation, Prepared, Receipt},
            quota, transaction,
        },
        challenge, message,
        message::{Conclusion, worker},
    },
    runtime::Sample,
};

impl Inner {
    pub(super) async fn prepare(
        &self,
        input: &Input,
        challenge: authentication::Receipt,
    ) -> Result<Preparation, Error> {
        let lease = self.context.connections().lease().await?;
        let mut option: Option<Prepared> = None;
        let error = match timeout(TRANSACTION_BUDGET, async {
            let client = lease.client();
            transaction::begin(client, input.operation_id).await?;
            let challenge = database::challenge::find(client, &self.keys, input)
                .await?
                .ok_or(Error::Facts {
                    stage: Stage::MessagePreparation,
                    rule: "the verified challenge must still exist",
                })?;
            if let Some(message) = database::message::find(client, challenge.id).await? {
                message.validate(&self.keys.message, self.sample().await?)?;
                client.batch_execute("COMMIT").await?;
                return Ok(Preparation::Original(message));
            }
            transaction::object(client, b"identity/mobile", input.mobile.as_bytes()).await?;
            let source = database::challenge::fetch_source(client, challenge.id).await?;
            transaction::object(client, b"message/source", source.to_string().as_bytes()).await?;
            transaction::object(client, b"message/deployment", b"permit").await?;
            let now = self.sample().await?;
            match challenge.status {
                Status::Passed {
                    solved_at,
                    finished_at,
                } if now >= finished_at
                    && now < solved_at.min(finished_at) + TimeDelta::seconds(300) => {}
                Status::Rejected { .. } => return Err(Error::Rejected),
                _ => return Err(Error::Verification),
            }
            let hash = self.keys.mobile.hash(&input.mobile)?;
            quota::verify(client, self.settings.message(), &hash, source, now).await?;
            let ordinal = database::message::next_ordinal(client).await?;
            let code = message::Code::generate()?;
            let message = Message::new(
                &self.keys.message,
                challenge.id,
                ordinal,
                now,
                self.settings.message(),
                &code,
            )?;
            option = Some(Prepared { message, code });
            let prepared = option.as_ref().ok_or(Error::Facts {
                stage: Stage::MessagePreparation,
                rule: "the original code and message must be retained before insertion",
            })?;
            prepared.message.insert(client).await?;
            client.batch_execute("COMMIT").await?;
            Ok::<_, Error>(Preparation::Ready(option.take().ok_or(Error::Facts {
                stage: Stage::MessagePreparation,
                rule: "a committed new permit must retain its original code and message",
            })?))
        })
        .await
        {
            Ok(Ok(preparation)) => {
                lease.release();
                return Ok(preparation);
            }
            Ok(Err(error)) => error,
            Err(error) => error.into(),
        };
        if error.rejection()
            && matches!(
                timeout(TRANSACTION_BUDGET, lease.client().batch_execute("ROLLBACK")).await,
                Ok(Ok(()))
            )
        {
            lease.release();
            return Err(error);
        }
        let cause = error.combine(lease.discard().await);
        let message = match self.recover_message(input, challenge.id).await {
            Ok(Some(message)) => message,
            Ok(None) => {
                return Err(Error::NotCommitted {
                    cause: Box::new(cause),
                });
            }
            Err(recovery) => {
                return Err(Error::Unknown {
                    cause: Box::new(cause),
                    recovery: Box::new(recovery),
                });
            }
        };
        let Some(prepared) = option else {
            return Ok(Preparation::Original(message));
        };
        if prepared.message.id() != message.id() {
            return Err(Error::Facts {
                stage: Stage::MessageRecovery,
                rule: "recovered message identity must match the original permit",
            });
        }
        if prepared.message.worker_id() != message.worker_id() {
            return Err(Error::Facts {
                stage: Stage::MessageRecovery,
                rule: "recovered worker must match the original execution",
            });
        }
        if !message.verify_code(&self.keys.message, &prepared.code)? {
            return Err(Error::Facts {
                stage: Stage::MessageRecovery,
                rule: "the retained code must verify against the recovered proof",
            });
        }
        if message.status() != message::Status::Sending {
            return Ok(Preparation::Original(message));
        }
        Ok(Preparation::Ready(Prepared {
            message,
            code: prepared.code,
        }))
    }

    async fn recover_message(
        &self,
        input: &Input,
        challenge_id: challenge::Id,
    ) -> Result<Option<Message>, Error> {
        let session = self.context.connections().recover().await?;
        let error = match timeout(TRANSACTION_BUDGET, async {
            transaction::begin(session.client(), input.operation_id).await?;
            let receipt = database::challenge::find(session.client(), &self.keys, input)
                .await?
                .ok_or(Error::Facts {
                    stage: Stage::MessageRecovery,
                    rule: "the original challenge must still exist",
                })?;
            if receipt.id != challenge_id {
                return Err(Error::Facts {
                    stage: Stage::MessageRecovery,
                    rule: "challenge identity must match the original receipt",
                });
            }
            let option = database::message::find(session.client(), challenge_id).await?;
            if let Some(message) = &option {
                message.validate(&self.keys.message, self.sample().await?)?;
            }
            session.client().batch_execute("COMMIT").await?;
            Ok::<_, Error>(option)
        })
        .await
        {
            Ok(Ok(option)) => {
                session.terminate().await?;
                return Ok(option);
            }
            Ok(Err(error)) => error,
            Err(error) => error.into(),
        };
        Err(error.combine(session.terminate().await))
    }

    pub(super) async fn finish_message(
        &self,
        input: &Input,
        challenge_id: challenge::Id,
        worker_id: worker::Id,
        conclusion: Conclusion,
    ) -> Result<Receipt, Error> {
        let (message, _) = self
            .change_message(
                input,
                challenge_id,
                &Change::Finish {
                    worker_id,
                    conclusion,
                },
            )
            .await?;
        Ok(Receipt::new(&message))
    }

    pub(super) async fn change_message(
        &self,
        input: &Input,
        challenge_id: challenge::Id,
        change: &Change<'_>,
    ) -> Result<(Message, Sample), Error> {
        let cause = match self.context.connections().lease().await {
            Ok(lease) => {
                let error = match timeout(
                    TRANSACTION_BUDGET,
                    self.write_message(lease.client(), input, challenge_id, change),
                )
                .await
                {
                    Ok(Ok(result)) => {
                        lease.release();
                        return Ok(result);
                    }
                    Ok(Err(error)) => error,
                    Err(error) => error.into(),
                };
                error.combine(lease.discard().await)
            }
            Err(error) => error.into(),
        };
        let recovery = async {
            let session = self.context.connections().recover().await?;
            let error = match timeout(
                TRANSACTION_BUDGET,
                self.write_message(session.client(), input, challenge_id, change),
            )
            .await
            {
                Ok(Ok(result)) => {
                    session.terminate().await?;
                    return Ok(result);
                }
                Ok(Err(error)) => error,
                Err(error) => error.into(),
            };
            Err(error.combine(session.terminate().await))
        }
        .await;
        match recovery {
            Ok(result) => Ok(result),
            Err(recovery) => Err(Error::Unknown {
                cause: Box::new(cause),
                recovery: Box::new(recovery),
            }),
        }
    }

    async fn write_message(
        &self,
        client: &Client,
        input: &Input,
        challenge_id: challenge::Id,
        change: &Change<'_>,
    ) -> Result<(Message, Sample), Error> {
        transaction::begin(client, input.operation_id).await?;
        let receipt = database::challenge::find(client, &self.keys, input)
            .await?
            .ok_or(Error::Facts {
                stage: Stage::MessageTransition,
                rule: "the original challenge must still exist",
            })?;
        if receipt.id != challenge_id {
            return Err(Error::Facts {
                stage: Stage::MessageTransition,
                rule: "challenge identity must match the original receipt",
            });
        }
        if matches!(change, Change::Claim { .. }) {
            transaction::object(client, b"identity/mobile", input.mobile.as_bytes()).await?;
        }
        let mut message =
            database::message::find(client, challenge_id)
                .await?
                .ok_or(Error::Facts {
                    stage: Stage::MessageTransition,
                    rule: "the original message must still exist",
                })?;
        let sample = self.context.observe().await?;
        message.validate(&self.keys.message, sample.observed_at())?;
        if message.status() != message::Status::Sending {
            client.batch_execute("COMMIT").await?;
            return Ok((message, sample));
        }
        let option = match change {
            Change::Claim { worker_id, code } => {
                if message.worker_id() != *worker_id {
                    return Err(Error::Facts {
                        stage: Stage::MessageClaim,
                        rule: "claimant must be the original worker",
                    });
                }
                if !message.verify_code(&self.keys.message, code)? {
                    return Err(Error::Facts {
                        stage: Stage::MessageClaim,
                        rule: "the retained code must verify against the message proof",
                    });
                }
                let hash = self.keys.mobile.hash(&input.mobile)?;
                if message.superseded(client, &hash).await?
                    || message
                        .expires_at()
                        .signed_duration_since(sample.observed_at())
                        < TimeDelta::seconds(60)
                {
                    Some(Conclusion::Failed)
                } else {
                    message.claim(client, sample.observed_at()).await?;
                    None
                }
            }
            Change::Finish {
                worker_id,
                conclusion,
            } => {
                if message.worker_id() != *worker_id {
                    return Err(Error::Facts {
                        stage: Stage::MessageConclusion,
                        rule: "the conclusion must belong to the original worker",
                    });
                }
                Some(*conclusion)
            }
            Change::Orphan => Some(message.orphaned()),
        };
        if let Some(conclusion) = option {
            message
                .finish(client, conclusion, sample.observed_at())
                .await?;
        }
        client.batch_execute("COMMIT").await?;
        Ok((message, sample))
    }
}
