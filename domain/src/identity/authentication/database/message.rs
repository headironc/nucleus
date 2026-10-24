//! Message facts, code pairing, claims and fixed queries shared by C1 consumers.

use chrono::{DateTime, TimeDelta, Utc};
use tokio::time::timeout;
use tokio_postgres::{Client, Row};

use crate::identity::{
    authentication::{Error, Stage, TRANSACTION_BUDGET},
    challenge, keys, message,
    message::{Conclusion, worker},
    mobile::hash::Hash,
};

pub(in crate::identity::authentication) struct Message {
    id: message::Id,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    status: message::Status,
    challenge_id: challenge::Id,
    ordinal: i64,
    worker_id: worker::Id,
    key_id: keys::Id,
    proof: message::proof::Proof,
    claimed_at: Option<DateTime<Utc>>,
}

impl Message {
    pub(in crate::identity::authentication) fn new(
        keys: &keys::message::Keys,
        challenge_id: challenge::Id,
        ordinal: i64,
        now: DateTime<Utc>,
        limits: &message::Limits,
        code: &message::Code,
    ) -> Result<Self, Error> {
        let id = message::Id::new();
        let worker_id = worker::Id::new();
        let expires_at = now
            .checked_add_signed(limits.lifetime())
            .ok_or(Error::Time {
                stage: Stage::MessagePreparation,
                rule: "trusted time plus the message lifetime must be representable",
            })?;
        let key = keys.active();
        let message = Self {
            id,
            challenge_id,
            ordinal,
            worker_id,
            created_at: now,
            expires_at,
            status: message::Status::Sending,
            key_id: key.id(),
            proof: key.prove(id, code)?,
            claimed_at: None,
        };
        // Construction precedes SQL so the caller can retain the original code across commit recovery.
        Ok(message)
    }

    pub(in crate::identity::authentication) async fn insert(
        &self,
        client: &Client,
    ) -> Result<(), Error> {
        client.execute("INSERT INTO identity.messages (id, challenge_id, ordinal, worker_id, created_at, expires_at) VALUES ($1,$2,$3,$4,$5,$6)",
            &[&self.id, &self.challenge_id, &self.ordinal, &self.worker_id, &self.created_at, &self.expires_at]).await?;
        client
            .execute(
                "INSERT INTO identity.checks (id, hash, key_id, expires_at) VALUES ($1,$2,$3,$4)",
                &[&self.id, &self.proof, &self.key_id, &self.expires_at],
            )
            .await?;
        Ok(())
    }

    pub(in crate::identity::authentication) fn id(&self) -> message::Id {
        self.id
    }
    pub(in crate::identity::authentication) fn status(&self) -> message::Status {
        self.status
    }
    pub(in crate::identity::authentication) fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
    pub(in crate::identity::authentication) fn claimed(&self) -> bool {
        self.claimed_at.is_some()
    }

    pub(in crate::identity::authentication) fn verify_code(
        &self,
        keys: &keys::message::Keys,
        code: &message::Code,
    ) -> Result<bool, Error> {
        Ok(keys
            .fetch(self.key_id)?
            .verify(self.id, code, &self.proof)?)
    }

    pub(in crate::identity::authentication) fn orphaned(&self) -> Conclusion {
        if self.claimed() {
            Conclusion::Unknown
        } else {
            Conclusion::Failed
        }
    }

    pub(in crate::identity::authentication) async fn claim(
        &mut self,
        client: &Client,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        if !self.claimed() {
            client
                .execute(
                    "INSERT INTO identity.claims (id, worker_id, created_at) VALUES ($1,$2,$3)",
                    &[&self.id, &self.worker_id, &now],
                )
                .await?;
            self.claimed_at = Some(now);
        }
        Ok(())
    }

    pub(in crate::identity::authentication) async fn finish(
        &mut self,
        client: &Client,
        conclusion: Conclusion,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        let status = conclusion.at(now);
        let changed = client.execute("UPDATE identity.messages SET status = $2, finished_at = $3 WHERE id = $1 AND status = 'sending'",
            &[&self.id, &status.as_ref(), &now]).await?;
        if changed != 1 {
            return Err(Error::Facts {
                stage: Stage::MessageConclusion,
                rule: "conclusion update must affect exactly one sending message",
            });
        }
        self.status = status;
        Ok(())
    }

    pub(in crate::identity::authentication) async fn superseded(
        &self,
        client: &Client,
        hash: &Hash,
    ) -> Result<bool, Error> {
        Ok(client.query_one(
            "SELECT EXISTS (SELECT 1 FROM identity.messages message JOIN identity.challenges challenge ON challenge.id = message.challenge_id
             WHERE (challenge.mobile).hash = $1 AND challenge.purpose = 'authentication' AND message.ordinal > $2)",
            &[hash, &self.ordinal],
        ).await?.try_get(0)?)
    }

    /// The caller deletes the parent Challenge in the same transaction.
    pub(in crate::identity::authentication) async fn delete(
        &self,
        client: &Client,
    ) -> Result<(), Error> {
        client
            .execute("DELETE FROM identity.checks WHERE id = $1", &[&self.id])
            .await?;
        client
            .execute("DELETE FROM identity.claims WHERE id = $1", &[&self.id])
            .await?;
        client
            .execute("DELETE FROM identity.messages WHERE id = $1", &[&self.id])
            .await?;
        Ok(())
    }

    pub(in crate::identity::authentication) fn worker_id(&self) -> worker::Id {
        self.worker_id
    }

    pub(in crate::identity::authentication) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub(in crate::identity::authentication) fn validate(
        &self,
        keys: &keys::message::Keys,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        keys.fetch(self.key_id)?;
        if self.created_at > now {
            return Err(Error::Time {
                stage: Stage::MessageValidation,
                rule: "created_at must not exceed trusted time",
            });
        }
        if self
            .status
            .finished_at()
            .is_some_and(|finished_at| finished_at > now)
        {
            return Err(Error::Time {
                stage: Stage::MessageValidation,
                rule: "finished_at must not exceed trusted time",
            });
        }
        if self.claimed_at.is_some_and(|claimed_at| claimed_at > now) {
            return Err(Error::Time {
                stage: Stage::MessageValidation,
                rule: "claimed_at must not exceed trusted time",
            });
        }
        Ok(())
    }
}

impl TryFrom<Row> for Message {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        let created_at = row.try_get("created_at")?;
        let expires_at = row.try_get("expires_at")?;
        let status = match (
            row.try_get::<_, &str>("status")?,
            row.try_get::<_, Option<DateTime<Utc>>>("finished_at")?,
        ) {
            ("sending", None) => message::Status::Sending,
            ("sent", Some(finished_at)) => message::Status::Sent { finished_at },
            ("failed", Some(finished_at)) => message::Status::Failed { finished_at },
            ("unknown", Some(finished_at)) => message::Status::Unknown { finished_at },
            _ => {
                return Err(Error::Facts {
                    stage: Stage::MessageDecode,
                    rule: "status and finished_at must form a valid message state",
                });
            }
        };
        let worker_id = row.try_get("worker_id")?;
        let option = match (
            row.try_get::<_, Option<worker::Id>>("claim_worker_id")?,
            row.try_get::<_, Option<DateTime<Utc>>>("claimed_at")?,
        ) {
            (None, None) => None,
            (Some(claim_worker_id), Some(claimed_at)) => {
                if claim_worker_id != worker_id {
                    return Err(Error::Facts {
                        stage: Stage::MessageDecode,
                        rule: "claim.worker_id must match message.worker_id",
                    });
                }
                Some(claimed_at)
            }
            _ => {
                return Err(Error::Facts {
                    stage: Stage::MessageDecode,
                    rule: "claim worker and claimed_at must both be present or absent",
                });
            }
        };
        let ordinal = row.try_get("ordinal")?;
        if expires_at <= created_at {
            return Err(Error::Facts {
                stage: Stage::MessageDecode,
                rule: "expires_at must follow created_at",
            });
        }
        if ordinal <= 0 {
            return Err(Error::Facts {
                stage: Stage::MessageDecode,
                rule: "ordinal must be positive",
            });
        }
        if row.try_get::<_, DateTime<Utc>>("check_expires_at")? != expires_at {
            return Err(Error::Facts {
                stage: Stage::MessageDecode,
                rule: "check.expires_at must equal message.expires_at",
            });
        }
        if status
            .finished_at()
            .is_some_and(|finished_at| finished_at < created_at)
        {
            return Err(Error::Facts {
                stage: Stage::MessageDecode,
                rule: "finished_at must not precede created_at",
            });
        }
        if let Some(claimed_at) = option {
            if claimed_at < created_at {
                return Err(Error::Facts {
                    stage: Stage::MessageDecode,
                    rule: "claimed_at must not precede created_at",
                });
            }
            if claimed_at > expires_at - TimeDelta::seconds(60) {
                return Err(Error::Facts {
                    stage: Stage::MessageDecode,
                    rule: "claim must leave at least sixty seconds before expires_at",
                });
            }
            if status
                .finished_at()
                .is_some_and(|finished_at| finished_at < claimed_at)
            {
                return Err(Error::Facts {
                    stage: Stage::MessageDecode,
                    rule: "finished_at must not precede claimed_at",
                });
            }
        }
        if matches!(
            status,
            message::Status::Sent { .. } | message::Status::Unknown { .. }
        ) && option.is_none()
        {
            return Err(Error::Facts {
                stage: Stage::MessageDecode,
                rule: "sent and unknown messages require a claim",
            });
        }
        Ok(Self {
            id: row.try_get("id")?,
            created_at,
            expires_at,
            status,
            challenge_id: row.try_get("challenge_id")?,
            ordinal,
            worker_id,
            key_id: row.try_get("key_id")?,
            proof: row.try_get("hash")?,
            claimed_at: option,
        })
    }
}

const SELECT: &str = "SELECT message.id, message.challenge_id, message.ordinal, message.worker_id,
    message.created_at, message.expires_at, message.status, message.finished_at,
    verification.hash, verification.key_id, verification.expires_at AS check_expires_at,
    claim.worker_id AS claim_worker_id, claim.created_at AS claimed_at
    FROM identity.messages message JOIN identity.checks verification ON verification.id = message.id
    LEFT JOIN identity.claims claim ON claim.id = message.id";

pub(in crate::identity::authentication) async fn next_ordinal(
    client: &Client,
) -> Result<i64, Error> {
    Ok(client
        .query_one("SELECT nextval('identity.ordinals') AS ordinal", &[])
        .await?
        .try_get("ordinal")?)
}

pub(in crate::identity::authentication) async fn lock(
    client: &Client,
    challenge_id: challenge::Id,
) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM identity.messages WHERE challenge_id = $1 FOR UPDATE",
            &[&challenge_id],
        )
        .await?;
    Ok(())
}

pub(in crate::identity::authentication) async fn find(
    client: &Client,
    challenge_id: challenge::Id,
) -> Result<Option<Message>, Error> {
    let sql = format!("{SELECT} WHERE message.challenge_id = $1");
    match client.query_opt(&sql, &[&challenge_id]).await? {
        Some(row) => Ok(Some(row.try_into()?)),
        None => {
            if client
                .query_opt(
                    "SELECT id FROM identity.messages WHERE challenge_id = $1",
                    &[&challenge_id],
                )
                .await?
                .is_some()
            {
                return Err(Error::Facts {
                    stage: Stage::MessageDecode,
                    rule: "a message must retain its paired check",
                });
            }
            Ok(None)
        }
    }
}

pub(in crate::identity::authentication) async fn verify_all(
    client: &Client,
    keys: &keys::message::Keys,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    timeout(
        TRANSACTION_BUDGET,
        client.batch_execute(&format!("DECLARE messages CURSOR FOR {SELECT}")),
    )
    .await??;
    loop {
        let complete = timeout(TRANSACTION_BUDGET, async {
                let rows = client.query("FETCH FORWARD 128 FROM messages", &[]).await?;
                let complete = rows.is_empty();
                for row in rows {
                    let message = Message::try_from(row)?;
                    message.validate(keys, now)?;
                    let passed: bool = client.query_one(
                        "SELECT status = 'passed' AND finished_at <= $2 AND $2 < least(solved_at, finished_at) + interval '300 seconds'
                         FROM identity.challenges WHERE id = $1", &[&message.challenge_id, &message.created_at],
                    ).await?.try_get(0)?;
                    if !passed {
                        return Err(Error::Facts {
                            stage: Stage::MessageHistory,
                            rule: "message creation must fall within the original passed proof's validity",
                        });
                    }
                }
                Ok::<_, Error>(complete)
            }).await??;
        if complete {
            break;
        }
    }
    timeout(TRANSACTION_BUDGET, client.batch_execute("CLOSE messages")).await??;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use tokio::runtime::Runtime;

    use super::{Message, worker};
    use crate::identity::{
        authentication::{
            Error, Stage,
            verification::tests::{Database, keys, server},
        },
        message,
    };

    #[test]
    fn message_diagnostics_distinguish_pairing_ownership_and_future_events() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let session = database.connections(&database.user).recover().await.expect("reader");
            let created_at = database.now().await;
            let expires_at = created_at + chrono::TimeDelta::seconds(300);
            let worker_id = worker::Id::new();
            let message_id = message::Id::new();
            let challenge_id = crate::identity::challenge::Id::new();
            for (check_expires_at, claim_worker_id, claimed_at, status, finished_at, now, option) in [
                (expires_at, worker_id, created_at, "sending", None, expires_at, None),
                (expires_at - chrono::TimeDelta::seconds(1), worker_id, created_at, "sending", None, expires_at, Some((Stage::MessageDecode, "check.expires_at"))),
                (expires_at, worker::Id::new(), created_at, "sending", None, expires_at, Some((Stage::MessageDecode, "worker_id"))),
                (expires_at, worker_id, created_at, "sending", None, created_at - chrono::TimeDelta::seconds(1), Some((Stage::MessageValidation, "created_at"))),
                (expires_at, worker_id, created_at, "failed", Some(created_at + chrono::TimeDelta::seconds(1)), created_at, Some((Stage::MessageValidation, "finished_at"))),
                (expires_at, worker_id, created_at + chrono::TimeDelta::seconds(1), "sending", None, created_at, Some((Stage::MessageValidation, "claimed_at"))),
            ] {
                let row = session.client().query_one(
                    "SELECT $1::uuid AS id, $2::uuid AS challenge_id, 1::bigint AS ordinal,
                        $3::uuid AS worker_id, $4::timestamptz AS created_at, $5::timestamptz AS expires_at,
                        $6::text AS status, $7::timestamptz AS finished_at, decode(repeat('00',32),'hex') AS hash,
                        0::bigint AS key_id, $8::timestamptz AS check_expires_at,
                        $9::uuid AS claim_worker_id, $10::timestamptz AS claimed_at",
                    &[&message_id, &challenge_id, &worker_id, &created_at, &expires_at, &status, &finished_at, &check_expires_at, &claim_worker_id, &claimed_at],
                ).await.expect("typed historical fixture");
                let result = match Message::try_from(row) {
                    Ok(message) => message.validate(&keys().message, now),
                    Err(error) => Err(error),
                };
                let Some((expected, field)) = option else {
                    result.expect("an expired but consistent message remains readable");
                    continue;
                };
                let error = result.expect_err("specific fact or time rule fails");
                let stage = match &error {
                    Error::Facts { stage, rule } | Error::Time { stage, rule } => {
                        assert!(rule.contains(field));
                        *stage
                    }
                    _ => panic!("a stored invariant has a stage and rule"),
                };
                assert_eq!(stage, expected);
                assert_eq!(matches!(error, Error::Time { .. }), expected == Stage::MessageValidation);
                assert!(error.unreliable());
                assert!(!format!("{error:?} {error}").contains(&message_id.to_string()));
            }
            session.terminate().await.expect("reader ended");
        });
    }
}
