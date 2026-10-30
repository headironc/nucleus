//! Message transactions and recovery; fixed queries and facts belong to authentication::database.

use chrono::TimeDelta;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Client;

use crate::{
    database::transaction::{Completion, Known, Outcome, commit, confirm},
    identity::{
        authentication::{
            self, Error, Inner, Input, Sender, Stage, Status, TRANSACTION_BUDGET,
            database::{
                self,
                message::{Binding, Message},
            },
            dispatch::{Change, Preparation, Prepared, Receipt},
            quota, transaction,
        },
        challenge, message,
        message::{Conclusion, worker},
    },
    kernel::time::Sample,
};

enum Transition {
    Original(Message),
    Inserted,
}

impl Inner {
    pub(super) async fn prepare(
        &self,
        input: &Input,
        challenge: authentication::Receipt,
    ) -> Result<(Preparation, Option<Box<Error>>), Error> {
        let lease = self.context.connections().lease().await?;
        let mut option: Option<Prepared> = None;
        let deadline = Instant::now() + TRANSACTION_BUDGET;
        let result = match timeout_at(deadline, async {
            let client = lease.client();
            transaction::begin(&self.context, client, input.operation_id).await?;
            let challenge = database::challenge::find_receipt(client, &self.keys, input)
                .await?
                .ok_or(Error::Facts {
                    stage: Stage::MessagePreparation,
                    rule: "the verified challenge must still exist",
                })?;
            input.lock_scope(client).await?;
            if let Some(message) =
                database::message::find_by_challenge(client, challenge.id).await?
            {
                self.authenticate_sender(client, input).await?;
                message.validate(&self.keys.message, self.now(client).await?)?;
                return Ok(Outcome::Known(Transition::Original(message)));
            }
            let binding = match &input.sender {
                Sender::Anonymous => Binding::Authentication,
                Sender::Current(actor) => {
                    let contact = self.contact(client, actor, &input.mobile).await?;
                    Binding::Current {
                        person_id: contact.person_id(),
                        operator_id: contact.revision_id(),
                    }
                }
                Sender::Transfer { actor, transfer_id } => {
                    let contact = self
                        .transfer_contact(client, actor, *transfer_id, &input.mobile)
                        .await?;
                    Binding::Transfer {
                        person_id: contact.person_id(),
                        operator_id: contact.operator_revision_id(),
                    }
                }
            };
            transaction::object(client, b"identity/mobile", input.mobile.as_bytes()).await?;
            let source = database::challenge::fetch_source(client, challenge.id).await?;
            transaction::object(client, b"message/source", source.to_string().as_bytes()).await?;
            transaction::object(client, b"message/deployment", b"permit").await?;
            let now = self.now(client).await?;
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
                binding,
            )?;
            option = Some(Prepared { message, code });
            let prepared = option.as_ref().ok_or(Error::Facts {
                stage: Stage::MessagePreparation,
                rule: "the original code and message must be retained before insertion",
            })?;
            prepared.message.insert(client).await?;
            Ok::<_, Error>(Outcome::Pending(Transition::Inserted))
        })
        .await
        {
            Ok(result) => result,
            Err(error) => Err(error.into()),
        };
        let error = match result {
            Ok(outcome) => {
                let completion =
                    commit(outcome, deadline, lease.client().batch_execute("COMMIT")).await;
                let (completion, cleanup) = completion.recycle(lease).await;
                match completion {
                    Completion::Known(Known { value, end }) => {
                        let preparation = match value {
                            Transition::Original(message) => Preparation::Original(message),
                            Transition::Inserted => Preparation::Ready(option.take().ok_or(Error::Facts {
                                stage: Stage::MessagePreparation,
                                rule: "a committed new permit must retain its original code and message",
                            })?),
                        };
                        let option = match end {
                            Ok(()) => cleanup.err().map(Error::from).map(Box::new),
                            Err(error) => Some(Box::new(Error::from(error).combine(cleanup))),
                        };
                        if option.as_ref().is_some_and(|cause| cause.unreliable()) {
                            self.registry.close();
                        }
                        return Ok((preparation, option));
                    }
                    Completion::Pending { error, .. } => Error::from(error).combine(cleanup),
                }
            }
            Err(error) => {
                if error.rejection() {
                    return Err(transaction::rollback(lease, error).await);
                }
                let error = error.combine(lease.discard().await);
                if error.unreliable() {
                    self.registry.close();
                }
                error
            }
        };
        let (readback, termination) = match self.recover_message(input, challenge.id).await {
            Ok(readback) => readback,
            Err(recovery) => {
                return Err(Error::Unknown {
                    cause: Box::new(error),
                    recovery: Box::new(recovery),
                });
            }
        };
        let commit = readback.end;
        let Some(message) = readback.value else {
            return Err(Error::NotCommitted {
                cause: Box::new(Error::MessageReadback {
                    cause: Box::new(error),
                    commit,
                    termination,
                }),
            });
        };
        if let Some(prepared) = &option
            && let Err(cause) = prepared.verify(&self.keys.message, &message)
        {
            if cause.unreliable() {
                self.registry.close();
            }
            return Err(Error::MessagePreparation {
                cause: Box::new(cause),
                preparation: Box::new(Error::MessageReadback {
                    cause: Box::new(error),
                    commit,
                    termination,
                }),
            });
        }
        let preparation = match option {
            Some(prepared) if message.status() == message::Status::Sending => {
                Preparation::Ready(Prepared {
                    message,
                    code: prepared.code,
                })
            }
            _ => Preparation::Original(message),
        };
        let option = if error.unreliable() || commit.is_err() || termination.is_err() {
            Some(Box::new(Error::MessageReadback {
                cause: Box::new(error),
                commit,
                termination,
            }))
        } else {
            None
        };
        Ok((preparation, option))
    }

    async fn recover_message(
        &self,
        input: &Input,
        challenge_id: challenge::Id,
    ) -> Result<
        (
            Known<Option<Message>>,
            Result<(), platform::database::Error>,
        ),
        Error,
    > {
        let session = self.context.connections().recover().await?;
        let deadline = Instant::now() + TRANSACTION_BUDGET;
        let result = match timeout_at(deadline, async {
            transaction::begin(&self.context, session.client(), input.operation_id).await?;
            let receipt = database::challenge::find_receipt(session.client(), &self.keys, input)
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
            let option =
                database::message::find_by_challenge(session.client(), challenge_id).await?;
            if let Some(message) = &option {
                message.validate(&self.keys.message, self.now(session.client()).await?)?;
            }
            Ok::<_, Error>(option)
        })
        .await
        {
            Ok(Ok(option)) => {
                Ok(confirm(option, deadline, session.client().batch_execute("COMMIT")).await)
            }
            Ok(Err(error)) => Err(error),
            Err(error) => Err(error.into()),
        };
        match result {
            Ok(completion) => Ok(completion.terminate(session).await),
            Err(error) => Err(error.combine(session.terminate().await)),
        }
    }

    pub(super) async fn finish_message(
        &self,
        input: &Input,
        challenge_id: challenge::Id,
        worker_id: worker::Id,
        conclusion: Conclusion,
    ) -> Result<(Receipt, Option<Box<Error>>), Error> {
        let (message, _, option) = self
            .change_message(
                input,
                challenge_id,
                &Change::Finish {
                    worker_id,
                    conclusion,
                },
            )
            .await?;
        Ok((Receipt::new(&message), option))
    }

    pub(super) async fn change_message(
        &self,
        input: &Input,
        challenge_id: challenge::Id,
        change: &Change<'_>,
    ) -> Result<(Message, Sample, Option<Box<Error>>), Error> {
        let cause = match self.context.connections().lease().await {
            Ok(lease) => {
                match self
                    .write_message(lease.client(), input, challenge_id, change, true)
                    .await
                {
                    Ok(completion) => {
                        let (completion, cleanup) = completion.recycle(lease).await;
                        match completion {
                            Completion::Known(Known {
                                value: (message, sample),
                                end,
                            }) => {
                                let option = match end {
                                    Ok(()) => cleanup.err().map(Error::from).map(Box::new),
                                    Err(cause) => {
                                        Some(Box::new(Error::from(cause).combine(cleanup)))
                                    }
                                };
                                if option.as_ref().is_some_and(|cause| cause.unreliable()) {
                                    self.registry.close();
                                }
                                return Ok((message, sample, option));
                            }
                            Completion::Pending { error, .. } => {
                                Error::from(error).combine(cleanup)
                            }
                        }
                    }
                    Err(error) => error.combine(lease.discard().await),
                }
            }
            Err(error) => error.into(),
        };
        let reliable = !cause.unreliable();
        if !reliable {
            self.registry.close();
        }
        let recovery = async {
            let session = self.context.connections().recover().await?;
            let result = self
                .write_message(session.client(), input, challenge_id, change, reliable)
                .await;
            match result {
                Ok(result) => Ok(result.terminate(session).await),
                Err(error) => Err(error.combine(session.terminate().await)),
            }
        }
        .await;
        match recovery {
            Ok((
                Completion::Known(Known {
                    value: (message, sample),
                    end: commit,
                }),
                termination,
            )) => {
                let option = if !reliable || commit.is_err() || termination.is_err() {
                    Some(Box::new(Error::MessageReadback {
                        cause: Box::new(cause),
                        commit,
                        termination,
                    }))
                } else {
                    None
                };
                let option = match option {
                    Some(cause)
                        if message.status() == message::Status::Sending && cause.unreliable() =>
                    {
                        return Err(*cause);
                    }
                    option => option,
                };
                Ok((message, sample, option))
            }
            Ok((Completion::Pending { error, .. }, termination)) => Err(Error::Unknown {
                cause: Box::new(cause),
                recovery: Box::new(Error::from(error).combine(termination)),
            }),
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
        reliable: bool,
    ) -> Result<Completion<(Message, Sample)>, Error> {
        let deadline = Instant::now() + TRANSACTION_BUDGET;
        let (message, sample, original) = timeout_at(deadline, async {
            transaction::begin(&self.context, client, input.operation_id).await?;
            let receipt = database::challenge::find_receipt(client, &self.keys, input)
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
                input.lock_scope(client).await?;
            }
            let eligible = if matches!(change, Change::Claim { .. }) {
                let eligible = match &input.sender {
                    Sender::Anonymous => true,
                    Sender::Current(actor) => {
                        match self.contact(client, actor, &input.mobile).await {
                            Ok(_) => true,
                            Err(error) if error.rejection() => false,
                            Err(error) => return Err(error),
                        }
                    }
                    Sender::Transfer { actor, transfer_id } => {
                        match self
                            .transfer_contact(client, actor, *transfer_id, &input.mobile)
                            .await
                        {
                            Ok(_) => true,
                            Err(error) if error.rejection() => false,
                            Err(error) => return Err(error),
                        }
                    }
                };
                transaction::object(client, b"identity/mobile", input.mobile.as_bytes()).await?;
                eligible
            } else {
                true
            };
            let mut message = database::message::find_by_challenge(client, challenge_id)
                .await?
                .ok_or(Error::Facts {
                    stage: Stage::MessageTransition,
                    rule: "the original message must still exist",
                })?;
            let sample = self.context.observe(client).await?;
            message.validate(&self.keys.message, sample.observed_at())?;
            if message.status() != message::Status::Sending || !reliable {
                return Ok((message, sample, true));
            }
            let mut original = false;
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
                    if !eligible
                        || message.superseded(client, &hash).await?
                        || message
                            .expires_at()
                            .signed_duration_since(sample.observed_at())
                            < TimeDelta::seconds(60)
                    {
                        Some(Conclusion::Failed)
                    } else {
                        original = message.claimed();
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
            Ok::<_, Error>((message, sample, original))
        })
        .await??;
        let outcome = if original {
            Outcome::Known((message, sample))
        } else {
            Outcome::Pending((message, sample))
        };
        Ok(commit(outcome, deadline, client.batch_execute("COMMIT")).await)
    }
}
