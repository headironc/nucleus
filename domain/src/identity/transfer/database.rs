use chrono::{DateTime, TimeDelta, Utc};
use platform::cryptography::Encoded;
use tokio_postgres::{Client, Row};
use uuid::Uuid;

use crate::{
    database::verification,
    identity::{
        current::{Actor, Contact},
        keys::Keys,
        message,
        mobile::envelope::Envelope,
        operation, person, session,
        transfer::{
            Assistance, BUDGET, Conclusion, Error, Id, Input, Proof, Target, Transfer,
            retention::{Candidate, Purpose},
        },
    },
    keys,
};

pub(crate) struct Original {
    operation_id: operation::Id,
    person_id: person::Id,
    operator_id: person::Id,
    revision_id: person::revision::Id,
    session_id: session::Id,
    source: Source,
    recorded_at: DateTime<Utc>,
    hash: [u8; 32],
    key_id: keys::Id,
    pub(super) conclusion: Conclusion,
}

enum Source {
    Current(message::Id),
    Assisted,
}

impl Original {
    pub(super) fn new(
        actor: &Actor,
        input: &Input,
        contact: &Contact,
        keys: &Keys,
        now: DateTime<Utc>,
        valid: bool,
    ) -> Result<Self, Error> {
        let proof = Encoded::new(&Proof {
            input,
            person_id: contact.person_id(),
            revision_id: contact.revision_id(),
            session_id: actor.session_id(),
        })?;
        let key = keys.input.active();
        let conclusion = if valid {
            Conclusion::Created(Transfer {
                id: Id(Uuid::new_v4()),
                expires_at: now
                    .checked_add_signed(TimeDelta::minutes(10))
                    .ok_or(Error::Facts {
                        rule: "transfer deadline must be representable",
                    })?,
            })
        } else {
            Conclusion::Rejected
        };
        Ok(Self {
            operation_id: input.operation_id,
            person_id: contact.person_id(),
            operator_id: contact.person_id(),
            revision_id: contact.revision_id(),
            session_id: actor.session_id(),
            source: Source::Current(input.message_id),
            recorded_at: now,
            hash: *key.sign(&proof).as_ref(),
            key_id: key.id(),
            conclusion,
        })
    }

    pub(super) fn deliver(
        self,
        keys: &Keys,
        actor: &Actor,
        input: &Input,
        now: DateTime<Utc>,
    ) -> Result<Conclusion, Error> {
        self.validate(keys, now)?;
        if !matches!(self.source, Source::Current(_)) {
            return Err(Error::Mismatch);
        }
        let proof = Encoded::new(&Proof {
            input,
            person_id: self.person_id,
            revision_id: self.revision_id,
            session_id: actor.session_id(),
        })?;
        if self.session_id != actor.session_id()
            || !keys.input.fetch(self.key_id)?.verify(&proof, &self.hash)
        {
            return Err(Error::Mismatch);
        }
        Ok(self.conclusion)
    }

    pub(crate) async fn establish(
        client: &Client,
        keys: &Keys,
        input: &Assistance<'_>,
        operator_id: person::revision::Id,
        now: DateTime<Utc>,
    ) -> Result<Transfer, Error> {
        let proof = Encoded::new(input)?;
        let key = keys.input.active();
        let transfer = Transfer {
            id: Id(Uuid::new_v4()),
            expires_at: now
                .checked_add_signed(TimeDelta::minutes(10))
                .ok_or(Error::Facts {
                    rule: "transfer deadline must be representable",
                })?,
        };
        client.execute("INSERT INTO identity.transfers (id, person_id, operator_id, revision_id, session_id, created_at, operation_id, hash, key_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            &[&transfer.id, &input.person_id, &operator_id, &input.revision_id, &input.session_id, &now, &input.operation_id, &key.sign(&proof).as_ref().as_slice(), &key.id()]).await?;
        Ok(transfer)
    }

    pub(crate) fn deliver_assisted(
        self,
        keys: &Keys,
        input: &Assistance<'_>,
        now: DateTime<Utc>,
    ) -> Result<Transfer, Error> {
        self.validate(keys, now)?;
        if !matches!(self.source, Source::Assisted)
            || !keys
                .input
                .fetch(self.key_id)?
                .verify(&Encoded::new(input)?, &self.hash)
        {
            return Err(Error::Mismatch);
        }
        match self.conclusion {
            Conclusion::Created(transfer) => Ok(transfer),
            Conclusion::Rejected => Err(Error::Facts {
                rule: "assisted establishment never consumes a verification code",
            }),
        }
    }

    pub(crate) fn operator_id(&self) -> person::Id {
        self.operator_id
    }

    pub(super) fn validate(&self, keys: &Keys, now: DateTime<Utc>) -> Result<(), Error> {
        keys.input.fetch(self.key_id)?;
        if self.recorded_at > now {
            return Err(Error::Facts {
                rule: "transfer conclusions must not exceed trusted time",
            });
        }
        Ok(())
    }

    pub(super) async fn insert(&self, client: &Client) -> Result<(), Error> {
        let Source::Current(message_id) = self.source else {
            return Err(Error::Facts {
                rule: "a self-service conclusion retains its original-phone code",
            });
        };
        match &self.conclusion {
            Conclusion::Created(transfer) => {
                client.execute("INSERT INTO identity.transfers (id, person_id, operator_id, revision_id, session_id, message_id, created_at, operation_id, hash, key_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                    &[&transfer.id, &self.person_id, &self.revision_id, &self.revision_id, &self.session_id, &message_id, &self.recorded_at, &self.operation_id, &&self.hash[..], &self.key_id]).await?;
            }
            Conclusion::Rejected => {
                client
                    .execute(
                        "UPDATE identity.confirmations SET failures = failures + 1 WHERE id = $1",
                        &[&message_id],
                    )
                    .await?;
                client.execute("INSERT INTO identity.rejections (purpose, operation_id, message_id, person_id, operator_id, revision_id, session_id, rejected_at, hash, key_id) VALUES ('transfer',$1,$2,$3,$4,$5,$6,$7,$8,$9)",
                    &[&self.operation_id, &message_id, &self.person_id, &self.revision_id, &self.revision_id, &self.session_id, &self.recorded_at, &&self.hash[..], &self.key_id]).await?;
            }
        }
        Ok(())
    }
}

impl Original {
    fn decode(
        row: Row,
        profiles: &std::collections::HashMap<person::revision::Id, person::revision::Profile>,
    ) -> Result<Self, Error> {
        let person_id = row.try_get("person_id")?;
        let revision_id = row.try_get("revision_id")?;
        let operator_id = row.try_get("operator_id")?;
        let subject = profiles
            .get(&revision_id)
            .ok_or(person::revision::Error::Missing)?;
        let operator = profiles
            .get(&operator_id)
            .ok_or(person::revision::Error::Missing)?;
        if subject.person_id() != person_id {
            return Err(person::revision::Error::Person.into());
        }
        let recorded_at: DateTime<Utc> = row.try_get("recorded_at")?;
        let conclusion = match row.try_get::<_, &str>("kind")? {
            "created" => Conclusion::Created(Transfer {
                id: row.try_get("id")?,
                expires_at: recorded_at
                    .checked_add_signed(TimeDelta::minutes(10))
                    .ok_or(Error::Facts {
                        rule: "transfer deadline must be representable",
                    })?,
            }),
            "rejected" => Conclusion::Rejected,
            _ => {
                return Err(Error::Facts {
                    rule: "original transfer has a recognized conclusion",
                });
            }
        };
        Ok(Self {
            operation_id: row.try_get("operation_id")?,
            person_id,
            operator_id: operator.person_id(),
            revision_id,
            session_id: row.try_get("session_id")?,
            source: match row.try_get::<_, Option<message::Id>>("message_id")? {
                Some(id) => Source::Current(id),
                None => Source::Assisted,
            },
            recorded_at,
            hash: row
                .try_get::<_, &[u8]>("hash")?
                .try_into()
                .map_err(Error::digest)?,
            key_id: row.try_get("key_id")?,
            conclusion,
        })
    }
}

const ORIGINAL: &str = "SELECT 'created'::text AS kind, transfer.id, transfer.operation_id, transfer.person_id, transfer.operator_id, transfer.revision_id, transfer.session_id, transfer.message_id, transfer.created_at AS recorded_at, transfer.hash, transfer.key_id FROM identity.transfers transfer
    UNION ALL SELECT 'rejected', NULL::uuid, rejection.operation_id, rejection.person_id, rejection.operator_id, rejection.revision_id, rejection.session_id, rejection.message_id, rejection.rejected_at, rejection.hash, rejection.key_id FROM identity.rejections rejection WHERE rejection.purpose = 'transfer'";

async fn restore(
    client: &Client,
    keys: crate::history::Keys<'_>,
    rows: Vec<Row>,
) -> Result<Vec<Original>, Error> {
    let ids = rows
        .iter()
        .map(|row| {
            Ok([
                row.try_get::<_, person::revision::Id>("revision_id")?,
                row.try_get("operator_id")?,
            ])
        })
        .collect::<Result<Vec<_>, tokio_postgres::Error>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let profiles = person::revision::fetch_many(client, keys, &ids).await?;
    rows.into_iter()
        .map(|row| Original::decode(row, &profiles))
        .collect()
}

pub(crate) async fn find_by_operation(
    client: &Client,
    keys: crate::history::Keys<'_>,
    operation_id: operation::Id,
) -> Result<Option<Original>, Error> {
    let row = client.query_opt(&format!("SELECT kind, id, operation_id, person_id, operator_id, revision_id, session_id, message_id, recorded_at, hash, key_id FROM ({ORIGINAL}) original WHERE operation_id = $1"), &[&operation_id]).await?;
    Ok(restore(client, keys, row.into_iter().collect())
        .await?
        .pop())
}

pub(super) struct Sender {
    person_id: person::Id,
    revision_id: person::revision::Id,
    session_id: session::Id,
    created_at: DateTime<Utc>,
    latest: bool,
}

impl TryFrom<Row> for Sender {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            person_id: row.try_get("person_id")?,
            revision_id: row.try_get("revision_id")?,
            session_id: row.try_get("session_id")?,
            created_at: row.try_get("created_at")?,
            latest: row.try_get("latest")?,
        })
    }
}

impl Sender {
    pub(super) fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub(super) fn session_id(&self) -> session::Id {
        self.session_id
    }

    pub(super) fn qualify(
        self,
        actor: &Actor,
        contact: &Target,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        if self.created_at > now {
            return Err(Error::Facts {
                rule: "transfer creation must not exceed trusted time",
            });
        }
        if self.person_id != contact.person_id()
            || self.revision_id != contact.revision_id()
            || self.session_id != actor.session_id()
            || !self.latest
            || now.signed_duration_since(self.created_at) >= TimeDelta::minutes(10)
        {
            return Err(Error::Verification);
        }
        Ok(())
    }
}

pub(super) async fn find_sender_by_id(client: &Client, id: Id) -> Result<Option<Sender>, Error> {
    client.query_opt("SELECT transfer.person_id, transfer.revision_id, transfer.session_id, transfer.created_at,
        NOT EXISTS (SELECT 1 FROM identity.transfers later WHERE later.person_id = transfer.person_id AND later.created_at > transfer.created_at) AS latest
        FROM identity.transfers transfer WHERE transfer.id = $1", &[&id]).await?.map(Sender::try_from).transpose()
}

pub(super) struct Confirmation {
    id: message::Id,
    person_id: person::Id,
    mobile: Envelope,
    proof: message::proof::Proof,
    key_id: keys::Id,
    failures: i16,
    expires_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    superseded: bool,
    consumed: bool,
}

impl TryFrom<Row> for Confirmation {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            id: row.try_get("id")?,
            person_id: row.try_get("person_id")?,
            mobile: row.try_get("mobile")?,
            proof: row.try_get("hash")?,
            key_id: row.try_get("key_id")?,
            failures: row.try_get("failures")?,
            expires_at: row.try_get("expires_at")?,
            created_at: row.try_get("created_at")?,
            superseded: row.try_get("superseded")?,
            consumed: row.try_get("consumed")?,
        })
    }
}

impl Confirmation {
    pub(super) fn qualify(
        &self,
        keys: &Keys,
        contact: &Contact,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        if self.created_at > now || now < contact.observed_at() {
            return Err(Error::Facts {
                rule: "confirmation and qualification must not exceed trusted time",
            });
        }
        if self.person_id != contact.person_id()
            || !self.mobile.matches(&keys.mobile, contact.mobile())?
            || self.superseded
            || self.consumed
            || self.expires_at <= now
            || self.failures >= 5
        {
            return Err(Error::Verification);
        }
        Ok(())
    }

    pub(super) fn verify(&self, keys: &Keys, code: &message::Code) -> Result<bool, Error> {
        Ok(keys
            .message
            .fetch(self.key_id)?
            .verify(self.id, code, &self.proof)?)
    }
}

pub(super) async fn lock_confirmation(
    client: &Client,
    message_id: message::Id,
) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM identity.confirmations WHERE id = $1 FOR UPDATE",
            &[&message_id],
        )
        .await?;
    Ok(())
}

pub(super) async fn find_confirmation(
    client: &Client,
    message_id: message::Id,
) -> Result<Option<Confirmation>, Error> {
    client.query_opt("SELECT confirmation.id, confirmation.person_id, challenge.mobile, confirmation.hash, confirmation.key_id, confirmation.failures, confirmation.expires_at, message.created_at,
        EXISTS (SELECT 1 FROM identity.confirmations later JOIN identity.messages delivery ON delivery.id = later.id JOIN identity.challenges original ON original.id = delivery.challenge_id WHERE later.person_id = confirmation.person_id AND original.purpose = 'mobile_change' AND (original.mobile).hash = (challenge.mobile).hash AND delivery.ordinal > message.ordinal) AS superseded,
        EXISTS (SELECT 1 FROM identity.transfers WHERE person_id = confirmation.person_id AND created_at >= message.created_at) AS consumed
        FROM identity.confirmations confirmation JOIN identity.messages message ON message.id = confirmation.id JOIN identity.challenges challenge ON challenge.id = message.challenge_id WHERE confirmation.id = $1 AND challenge.purpose = 'mobile_change'", &[&message_id]).await?.map(Confirmation::try_from).transpose()
}

pub(super) async fn verify(
    client: &Client,
    keys: &Keys,
    history: crate::history::Keys<'_>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    verification::Stage::TransfersOpen
        .query(
            BUDGET,
            client.batch_execute(&format!("DECLARE transfers CURSOR FOR {ORIGINAL}")),
        )
        .await??;
    loop {
        let complete = verification::Stage::TransfersBatch
            .batch(BUDGET, async {
                let rows = client
                    .query("FETCH FORWARD 128 FROM transfers", &[])
                    .await?;
                let complete = rows.is_empty();
                for original in restore(client, history, rows).await? {
                    original.validate(keys, now)?;
                }
                Ok::<_, Error>(complete)
            })
            .await??;
        if complete {
            break;
        }
    }
    verification::Stage::TransfersClose
        .query(BUDGET, client.batch_execute("CLOSE transfers"))
        .await??;
    let valid: bool = verification::Stage::TransferRelations.query(BUDGET, client.query_one("SELECT NOT EXISTS (SELECT 1 FROM identity.confirmations confirmation WHERE confirmation.failures < (SELECT count(*) FROM identity.rejections WHERE message_id = confirmation.id) OR (confirmation.expires_at > $1 AND confirmation.failures <> (SELECT count(*) FROM identity.rejections WHERE message_id = confirmation.id)))", &[&now])).await??.try_get(0)?;
    if !valid {
        return Err(Error::Facts {
            rule: "confirmation failures preserve retained original rejections",
        });
    }
    Ok(())
}

pub(super) enum Deletion {
    Transfer(Id),
    Rejection {
        purpose: Purpose,
        operation_id: operation::Id,
        message_id: message::Id,
        rejected_at: DateTime<Utc>,
    },
}

impl Deletion {
    pub(super) async fn remove(&self, client: &Client) -> Result<(), Error> {
        match self {
            Self::Transfer(id) => {
                client
                    .execute("DELETE FROM identity.transfers WHERE id = $1", &[id])
                    .await?;
            }
            Self::Rejection {
                purpose,
                operation_id,
                message_id,
                rejected_at,
            } => {
                client.execute("DELETE FROM identity.rejections WHERE purpose = $4 AND operation_id = $1 AND message_id = $2 AND rejected_at = $3", &[operation_id, message_id, rejected_at, &purpose.as_str()]).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn complete(&self, client: &Client) -> Result<bool, Error> {
        Ok(match self {
            Self::Transfer(id) => client.query_one("SELECT NOT EXISTS (SELECT 1 FROM identity.transfers WHERE id = $1)", &[id]).await?.try_get(0)?,
            Self::Rejection { purpose, operation_id, message_id, rejected_at } => client.query_one("SELECT NOT EXISTS (SELECT 1 FROM identity.rejections WHERE purpose = $4 AND operation_id = $1 AND message_id = $2 AND rejected_at = $3)", &[operation_id, message_id, rejected_at, &purpose.as_str()]).await?.try_get(0)?,
        })
    }
}

const EXPIRED: &str = "SELECT 'transfer'::text AS purpose, transfer.operation_id FROM identity.transfers transfer WHERE transfer.created_at + interval '24 hours 10 minutes' <= $3 AND NOT EXISTS (SELECT 1 FROM identity.messages WHERE id = transfer.message_id) AND NOT EXISTS (SELECT 1 FROM identity.challenges WHERE transfer_id = transfer.id)
    UNION SELECT rejection.purpose, rejection.operation_id FROM identity.rejections rejection JOIN identity.confirmations confirmation ON confirmation.id = rejection.message_id WHERE confirmation.expires_at + interval '24 hours' <= $3";

pub(super) async fn fetch_candidates(
    client: &Client,
    cursor: Option<Candidate>,
    now: DateTime<Utc>,
) -> Result<Vec<Candidate>, Error> {
    let purpose = cursor.map(|candidate| candidate.purpose.as_str());
    let operation_id = cursor.map(|candidate| candidate.operation_id);
    let rows = client.query(&format!("SELECT purpose, operation_id FROM ({EXPIRED}) candidate WHERE ($1::text IS NULL OR (purpose, operation_id) > ($1, $2::uuid)) ORDER BY purpose, operation_id LIMIT 128"), &[&purpose, &operation_id, &now]).await?;
    rows.into_iter()
        .map(|row| {
            let purpose = match row.try_get::<_, &str>("purpose")? {
                "transfer" => Purpose::Transfer,
                "completion" => Purpose::Completion,
                _ => {
                    return Err(Error::Facts {
                        rule: "mobile rejection purpose has a retained operation family",
                    });
                }
            };
            Ok(Candidate {
                purpose,
                operation_id: row.try_get("operation_id")?,
            })
        })
        .collect()
}

pub(super) async fn find_deletion_by_operation(
    client: &Client,
    candidate: Candidate,
    now: DateTime<Utc>,
) -> Result<Option<Deletion>, Error> {
    let operation_id = candidate.operation_id;
    if matches!(candidate.purpose, Purpose::Transfer)
        && let Some(row) = client.query_opt("SELECT id FROM identity.transfers transfer WHERE operation_id = $1 AND created_at + interval '24 hours 10 minutes' <= $2 AND NOT EXISTS (SELECT 1 FROM identity.messages WHERE id = transfer.message_id) AND NOT EXISTS (SELECT 1 FROM identity.challenges WHERE transfer_id = transfer.id)", &[&operation_id, &now]).await? {
        return Ok(Some(Deletion::Transfer(row.try_get(0)?)));
    }
    if let Some(row) = client.query_opt("SELECT rejection.message_id, rejection.rejected_at FROM identity.rejections rejection JOIN identity.confirmations confirmation ON confirmation.id = rejection.message_id WHERE rejection.purpose = $3 AND rejection.operation_id = $1 AND confirmation.expires_at + interval '24 hours' <= $2", &[&operation_id, &now, &candidate.purpose.as_str()]).await? {
        return Ok(Some(Deletion::Rejection { purpose: candidate.purpose, operation_id, message_id: row.try_get(0)?, rejected_at: row.try_get(1)? }));
    }
    Ok(None)
}
