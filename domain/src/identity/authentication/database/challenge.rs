//! Challenge facts, original-input checks and fixed queries shared by C1 consumers.

use std::net::IpAddr;

use chrono::{DateTime, TimeDelta, Utc};
use tokio::time::timeout;
use tokio_postgres::{Client, Row};

use crate::{
    database::Query,
    identity::{
        authentication::{Error, Input, Keys, Receipt, Stage, Status, TRANSACTION_BUDGET},
        challenge::{self, Purpose, mobile, proof::Proof},
        keys, operation,
    },
};

const COLUMNS: &str = "id, operation_id, purpose, hash, key_id, mobile, source, calls, created_at, expires_at, status, solved_at, finished_at";

pub(in crate::identity::authentication) struct Challenge {
    id: challenge::Id,
    operation_id: operation::Id,
    purpose: Purpose,
    proof: Proof,
    key_id: keys::Id,
    mobile: mobile::Mobile,
    source: IpAddr,
    calls: i16,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    status: Status,
}

impl TryFrom<Row> for Challenge {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            operation_id: row.try_get("operation_id")?,
            purpose: row.try_get("purpose")?,
            proof: row.try_get("hash")?,
            key_id: row.try_get("key_id")?,
            mobile: row.try_get("mobile")?,
            source: row.try_get("source")?,
            calls: row.try_get("calls")?,
            created_at: row.try_get("created_at")?,
            expires_at: row.try_get("expires_at")?,
            status: Status::try_from(&row)?,
        })
    }
}

impl TryFrom<&Row> for Status {
    type Error = Error;

    fn try_from(row: &Row) -> Result<Self, Self::Error> {
        match (
            row.try_get::<_, &str>("status")?,
            row.try_get::<_, Option<DateTime<Utc>>>("solved_at")?,
            row.try_get::<_, Option<DateTime<Utc>>>("finished_at")?,
        ) {
            ("calling", None, None) => Ok(Self::Calling),
            ("retryable", None, Some(finished_at)) => Ok(Self::Retryable { finished_at }),
            ("passed", Some(solved_at), Some(finished_at)) => Ok(Self::Passed {
                solved_at,
                finished_at,
            }),
            ("rejected", None, Some(finished_at)) => Ok(Self::Rejected { finished_at }),
            ("inconclusive", None, Some(finished_at)) => Ok(Self::Inconclusive { finished_at }),
            _ => Err(Error::Facts {
                stage: Stage::ChallengeDecode,
                rule: "status, solved_at and finished_at must form a valid challenge state",
            }),
        }
    }
}

impl Challenge {
    pub(in crate::identity::authentication) fn id(&self) -> challenge::Id {
        self.id
    }

    pub(in crate::identity::authentication) fn status(&self) -> Status {
        self.status
    }

    pub(in crate::identity::authentication) fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    pub(in crate::identity::authentication) fn source(&self) -> IpAddr {
        self.source
    }

    pub(in crate::identity::authentication) fn mobile(
        &self,
        keys: &Keys,
    ) -> Result<crate::identity::mobile::Mobile, Error> {
        Ok(self.mobile.open(&keys.encryption, &keys.mobile, self.id)?)
    }

    pub(in crate::identity::authentication) fn validate_at(
        &self,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        if self.created_at > now {
            return Err(Error::Time {
                stage: Stage::ChallengeHistory,
                rule: "created_at must not exceed trusted time",
            });
        }
        if self
            .status
            .finished_at()
            .is_some_and(|finished_at| finished_at > now)
        {
            return Err(Error::Time {
                stage: Stage::ChallengeHistory,
                rule: "finished_at must not exceed trusted time",
            });
        }
        Ok(())
    }

    pub(in crate::identity::authentication) async fn conclude(
        &self,
        client: &Client,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        let changed = client.execute(
            "UPDATE identity.challenges SET status = 'inconclusive', finished_at = $2 WHERE id = $1 AND status = $3 AND calls = $4",
            &[&self.id, &now, &self.status, &self.calls],
        ).await?;
        if changed != 1 {
            return Err(Error::Facts {
                stage: Stage::Retention,
                rule: "the original unresolved challenge must still exist",
            });
        }
        Ok(())
    }

    pub(in crate::identity::authentication) async fn lock(
        &self,
        client: &Client,
    ) -> Result<(), Error> {
        client
            .query_opt(
                "SELECT id FROM identity.challenges WHERE id = $1 FOR UPDATE",
                &[&self.id],
            )
            .await?;
        Ok(())
    }

    pub(in crate::identity::authentication) async fn delete(
        &self,
        client: &Client,
    ) -> Result<(), Error> {
        client
            .execute("DELETE FROM identity.challenges WHERE id = $1", &[&self.id])
            .await?;
        Ok(())
    }

    fn validate(&self, keys: &Keys) -> Result<(), Error> {
        keys.challenge.fetch(self.key_id)?;
        self.mobile.open(&keys.encryption, &keys.mobile, self.id)?;
        if self.purpose != Purpose::Authentication {
            return Err(Error::Facts {
                stage: Stage::ChallengeValidation,
                rule: "purpose must be authentication",
            });
        }
        if !(1..=2).contains(&self.calls) {
            return Err(Error::Facts {
                stage: Stage::ChallengeValidation,
                rule: "calls must be one or two",
            });
        }
        if self.created_at.checked_add_signed(TimeDelta::seconds(10)) != Some(self.expires_at) {
            return Err(Error::Facts {
                stage: Stage::ChallengeValidation,
                rule: "expires_at must be ten seconds after created_at",
            });
        }
        if self.source != self.source.to_canonical() {
            return Err(Error::Facts {
                stage: Stage::ChallengeValidation,
                rule: "source must use its canonical IP representation",
            });
        }
        if self
            .status
            .finished_at()
            .is_some_and(|finished_at| finished_at < self.created_at)
        {
            return Err(Error::Facts {
                stage: Stage::ChallengeValidation,
                rule: "finished_at must not precede created_at",
            });
        }
        match self.status {
            Status::Calling | Status::Rejected { .. } | Status::Inconclusive { .. } => {}
            Status::Retryable { finished_at } => {
                if self.calls != 1 {
                    return Err(Error::Facts {
                        stage: Stage::ChallengeValidation,
                        rule: "retryable requires exactly one occupied call",
                    });
                }
                if finished_at >= self.expires_at {
                    return Err(Error::Facts {
                        stage: Stage::ChallengeValidation,
                        rule: "retryable finished_at must precede expires_at",
                    });
                }
            }
            Status::Passed {
                solved_at,
                finished_at,
            } => {
                if !finished_at
                    .checked_add_signed(TimeDelta::seconds(2))
                    .is_some_and(|limit| solved_at <= limit)
                {
                    return Err(Error::Facts {
                        stage: Stage::ChallengeValidation,
                        rule: "solved_at must fit the two-second tolerance after finished_at",
                    });
                }
                if !solved_at
                    .min(finished_at)
                    .checked_add_signed(TimeDelta::seconds(300))
                    .is_some_and(|expires_at| finished_at < expires_at)
                {
                    return Err(Error::Facts {
                        stage: Stage::ChallengeValidation,
                        rule: "passed proof must be younger than 300 seconds at finished_at",
                    });
                }
            }
        }
        Ok(())
    }

    fn verify(self, keys: &Keys, input: &Input) -> Result<Receipt, Error> {
        if !keys
            .challenge
            .fetch(self.key_id)?
            .verify(&input.token, &self.proof)?
        {
            return Err(Error::Proof);
        }
        self.validate(keys)?;
        if self.operation_id != input.operation_id
            || !self.mobile.matches(&keys.mobile, &input.mobile)?
        {
            return Err(Error::Mismatch);
        }
        Ok(Receipt {
            id: self.id,
            created_at: self.created_at,
            expires_at: self.expires_at,
            calls: self.calls,
            status: self.status,
        })
    }
}

pub(in crate::identity::authentication) async fn find(
    client: &Client,
    keys: &Keys,
    input: &Input,
) -> Result<Option<Receipt>, Error> {
    let option = inspect(client, input.operation_id).await?;
    option
        .map(|challenge| challenge.verify(keys, input))
        .transpose()
}

pub(in crate::identity::authentication) async fn inspect(
    client: &Client,
    operation_id: operation::Id,
) -> Result<Option<Challenge>, Error> {
    let sql = format!("SELECT {COLUMNS} FROM identity.challenges WHERE operation_id = $1");
    client
        .query_opt(&sql, &[&operation_id])
        .await?
        .map(Challenge::try_from)
        .transpose()
}

pub(in crate::identity::authentication) async fn fetch_source(
    client: &Client,
    challenge_id: challenge::Id,
) -> Result<IpAddr, Error> {
    Ok(client
        .query_one(
            "SELECT source FROM identity.challenges WHERE id = $1",
            &[&challenge_id],
        )
        .await?
        .try_get(0)?)
}

pub(in crate::identity::authentication) async fn fetch_candidates(
    client: &Client,
    option: Option<operation::Id>,
    now: DateTime<Utc>,
) -> Result<Vec<operation::Id>, Error> {
    let rows = client.query(
        "SELECT operation_id FROM identity.challenges challenge
         WHERE ($1::uuid IS NULL OR operation_id > $1) AND (
            status = 'calling' OR (status = 'retryable' AND expires_at <= $2)
            OR greatest(expires_at, finished_at) <= $2::timestamptz - interval '24 hours'
            OR EXISTS (SELECT 1 FROM identity.messages WHERE challenge_id = challenge.id AND status = 'sending'))
         ORDER BY operation_id LIMIT 128", &[&option, &now],
    ).await?;
    Ok(rows
        .iter()
        .map(|row| row.try_get(0))
        .collect::<Result<_, _>>()?)
}

pub(in crate::identity::authentication) async fn verify(
    client: &Client,
    keys: &Keys,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<Challenge>, Error> {
    let option = inspect(client, operation_id).await?;
    if let Some(challenge) = &option {
        challenge.validate(keys)?;
        challenge.validate_at(now)?;
    }
    Ok(option)
}

pub(in crate::identity::authentication) async fn deleted(
    client: &Client,
    challenge_id: challenge::Id,
    option: Option<crate::identity::message::Id>,
) -> Result<bool, Error> {
    Ok(client
        .query_one(
            "SELECT NOT EXISTS (SELECT 1 FROM identity.challenges WHERE id = $1)
            AND NOT EXISTS (SELECT 1 FROM identity.messages WHERE challenge_id = $1 OR id = $2)
            AND NOT EXISTS (SELECT 1 FROM identity.checks WHERE id = $2)
            AND NOT EXISTS (SELECT 1 FROM identity.claims WHERE id = $2)",
            &[&challenge_id, &option],
        )
        .await?
        .try_get(0)?)
}

pub(in crate::identity::authentication) async fn insert(
    client: &Client,
    keys: &Keys,
    input: &Input,
    now: DateTime<Utc>,
) -> Result<Receipt, Error> {
    for key in keys.challenge.all() {
        if client
            .query_opt(
                "SELECT id FROM identity.challenges WHERE key_id = $1 AND hash = $2",
                &[&key.id(), &key.prove(&input.token)?],
            )
            .await?
            .is_some()
        {
            return Err(Error::Occupied);
        }
    }
    let receipt = Receipt {
        id: challenge::Id::new(),
        created_at: now,
        expires_at: now
            .checked_add_signed(TimeDelta::seconds(10))
            .ok_or(Error::Time {
                stage: Stage::ChallengeAdmission,
                rule: "created_at plus the challenge lifetime must be representable",
            })?,
        calls: 1,
        status: Status::Calling,
    };
    let sql = "INSERT INTO identity.challenges (id, operation_id, purpose, hash, key_id, mobile, source, calls, created_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $9)";
    let error = match client
        .execute(
            sql,
            &[
                &receipt.id,
                &input.operation_id,
                &Purpose::Authentication,
                &keys.challenge.active().prove(&input.token)?,
                &keys.challenge.active().id(),
                &mobile::Mobile::seal(&keys.encryption, &keys.mobile, receipt.id, &input.mobile)?,
                &input.source,
                &receipt.created_at,
                &receipt.expires_at,
            ],
        )
        .await
    {
        Ok(_) => return Ok(receipt),
        Err(error) => Query(error),
    };
    // The stable token lock covers retained versions; this is its final database protection.
    if error.is_constraint(
        &tokio_postgres::error::SqlState::UNIQUE_VIOLATION,
        "identity",
        "challenges",
        "challenges_key_id_hash_key",
    ) {
        return Err(Error::Occupied);
    }
    Err(error.into())
}

pub(in crate::identity::authentication) async fn verify_all(
    client: &Client,
    keys: &Keys,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    // A cursor bounds retained-history memory while holding the exclusive barrier.
    timeout(
        TRANSACTION_BUDGET,
        client.batch_execute(&format!(
            "DECLARE challenges CURSOR FOR SELECT {COLUMNS} FROM identity.challenges"
        )),
    )
    .await??;
    loop {
        // Bound each batch without making retained history exhaust a total startup budget.
        let complete = timeout(TRANSACTION_BUDGET, async {
            let rows = client
                .query("FETCH FORWARD 128 FROM challenges", &[])
                .await?;
            let complete = rows.is_empty();
            for row in rows {
                let challenge = Challenge::try_from(row)?;
                challenge.validate(keys)?;
                challenge.validate_at(now)?;
            }
            Ok::<_, Error>(complete)
        })
        .await??;
        if complete {
            break;
        }
    }
    timeout(TRANSACTION_BUDGET, client.batch_execute("CLOSE challenges")).await??;
    Ok(())
}

pub(in crate::identity::authentication) async fn update(
    client: &Client,
    input: &Input,
    previous: Receipt,
    next: Receipt,
) -> Result<(), Error> {
    let changed = client.execute(
        "UPDATE identity.challenges SET calls = $2, status = $3, solved_at = $4, finished_at = $5 WHERE operation_id = $1 AND calls = $6 AND status = $7",
        &[&input.operation_id, &next.calls, &next.status, &next.status.solved_at(), &next.status.finished_at(), &previous.calls, &previous.status],
    ).await?;
    if changed != 1 {
        return Err(Error::Facts {
            stage: Stage::ChallengeTransition,
            rule: "state update must affect exactly one challenge with the previous calls and status",
        });
    }
    Ok(())
}
