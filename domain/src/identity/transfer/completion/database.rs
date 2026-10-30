//! The short-lived verification facts used by mobile completion.

use chrono::{DateTime, TimeDelta, Utc};
use platform::cryptography::Encoded;
use tokio_postgres::{Client, Row};

use crate::{
    database::verification,
    history,
    identity::{
        account,
        current::Actor,
        keys::Keys,
        message,
        mobile::envelope::Envelope,
        operation, person, session,
        transfer::{
            BUDGET, Error, Id, Target,
            completion::{self, Canonical, Input, Proof, State},
        },
    },
    kernel, keys,
};

pub(super) struct Rejected {
    id: Id,
    account_id: account::Id,
    operation_id: operation::Id,
    person_id: person::Id,
    operator_id: person::Id,
    operator_revision_id: person::revision::Id,
    revision_id: person::revision::Id,
    session_id: session::Id,
    message_id: message::Id,
    recorded_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    hash: [u8; 32],
    key_id: keys::Id,
}

impl Rejected {
    pub(super) fn new(
        actor: &Actor,
        input: &Input,
        target: &Target,
        confirmation: &Confirmation,
        keys: &Keys,
        now: DateTime<Utc>,
    ) -> Result<Self, Error> {
        let proof = Encoded::new(&Proof {
            input,
            transfer_id: confirmation.transfer_id,
            person_id: target.person_id(),
            operator_id: target.operator_id(),
            session_id: actor.session_id(),
        })?;
        let key = keys.input.active();
        Ok(Self {
            id: confirmation.transfer_id,
            account_id: target.account_id(),
            operation_id: input.operation_id,
            person_id: target.person_id(),
            operator_id: target.operator_id(),
            operator_revision_id: confirmation.operator_id,
            revision_id: target.revision_id(),
            session_id: actor.session_id(),
            message_id: input.message_id,
            recorded_at: now,
            expires_at: confirmation.retained_until()?,
            hash: *key.sign(&proof).as_ref(),
            key_id: key.id(),
        })
    }

    fn decode(
        row: Row,
        profiles: &std::collections::HashMap<person::revision::Id, person::revision::Profile>,
    ) -> Result<Self, Error> {
        let decode = |error| Error::History(history::Error::decode(error));
        if !row.try_get::<_, bool>("valid").map_err(decode)? {
            return Err(Error::Facts {
                rule: "a completion rejection retains its original verification bindings",
            });
        }
        let person_id = row.try_get("person_id").map_err(decode)?;
        let revision_id = row.try_get("revision_id").map_err(decode)?;
        let operator_revision_id = row.try_get("operator_revision_id").map_err(decode)?;
        let subject = profiles
            .get(&revision_id)
            .ok_or(person::revision::Error::Missing)?;
        let operator = profiles
            .get(&operator_revision_id)
            .ok_or(person::revision::Error::Missing)?;
        if subject.person_id() != person_id {
            return Err(person::revision::Error::Person.into());
        }
        Ok(Self {
            id: row.try_get("id").map_err(decode)?,
            account_id: row.try_get("account_id").map_err(decode)?,
            operation_id: row.try_get("operation_id").map_err(decode)?,
            person_id,
            operator_id: operator.person_id(),
            operator_revision_id,
            revision_id,
            session_id: row.try_get("session_id").map_err(decode)?,
            message_id: row.try_get("message_id").map_err(decode)?,
            recorded_at: row.try_get("recorded_at").map_err(decode)?,
            expires_at: row.try_get("expires_at").map_err(decode)?,
            hash: row
                .try_get::<_, &[u8]>("hash")
                .map_err(decode)?
                .try_into()
                .map_err(Error::digest)?,
            key_id: row.try_get("key_id").map_err(decode)?,
        })
    }

    pub(super) fn operation_id(&self) -> operation::Id {
        self.operation_id
    }

    pub(super) fn validate(&self, keys: &Keys, now: DateTime<Utc>) -> Result<(), Error> {
        keys.input.fetch(self.key_id)?;
        if self.recorded_at > now || self.expires_at <= self.recorded_at {
            return Err(Error::Facts {
                rule: "a code rejection preserves its trusted time and retention",
            });
        }
        Ok(())
    }

    pub(super) fn paired(
        &self,
        record: &kernel::completion::Record,
    ) -> Result<(), completion::Error> {
        if record.operation_id() != self.operation_id
            || record.person_id() != self.operator_id
            || record.created_at() != self.recorded_at
            || record.expires_at() != Some(self.expires_at)
            || record.target() != self.account_id.to_string()
            || record.action()
                != if self.operator_id == self.person_id {
                    "self"
                } else {
                    "assisted"
                }
        {
            return Err(completion::Error::corrupt(
                "a mobile rejection and completion retain the same original",
            ));
        }
        Ok(())
    }

    pub(super) fn matches(&self, keys: &Keys, actor: &Actor, input: &Input) -> Result<(), Error> {
        if actor.session_id() != self.session_id
            || input.subject.account_id(actor) != self.account_id
        {
            return Err(Error::Mismatch);
        }
        self.check_input(keys, input, actor.session_id())
    }

    fn check_input(
        &self,
        keys: &Keys,
        input: &Input,
        session_id: session::Id,
    ) -> Result<(), Error> {
        let proof = Encoded::new(&Proof {
            input,
            transfer_id: self.id,
            person_id: self.person_id,
            operator_id: self.operator_id,
            session_id,
        })?;
        if input.message_id != self.message_id
            || !keys.input.fetch(self.key_id)?.verify(&proof, &self.hash)
        {
            return Err(Error::Mismatch);
        }
        Ok(())
    }

    pub(super) fn verify_input(
        &self,
        keys: &Keys,
        record: &kernel::completion::Record,
        input: Canonical,
    ) -> Result<(), completion::Error> {
        if input.session_id != self.session_id {
            return Err(completion::Error::corrupt(
                "a rejection keeps its original session",
            ));
        }
        let input = Input {
            subject: match record.action() {
                "self" => completion::Subject::Current,
                "assisted" => completion::Subject::Assisted(self.account_id),
                _ => {
                    return Err(completion::Error::corrupt(
                        "a rejection has a declared mobile action",
                    ));
                }
            },
            operation_id: self.operation_id,
            mobile: input.mobile,
            message_id: input.message_id,
            code: input.code,
        };
        self.check_input(keys, &input, self.session_id)
            .map_err(|error| match error {
                Error::Mismatch => completion::Error::corrupt(
                    "the canonical rejected input matches its code-counter proof",
                ),
                error => error.into(),
            })
    }

    pub(super) async fn insert(&self, client: &Client) -> Result<(), Error> {
        if client
            .execute(
                "UPDATE identity.confirmations SET failures=failures+1 WHERE id=$1 AND failures<5",
                &[&self.message_id],
            )
            .await?
            != 1
        {
            return Err(Error::Facts {
                rule: "the locked verification accepts exactly one failed attempt",
            });
        }
        client.execute(
            "INSERT INTO identity.rejections (purpose,operation_id,account_id,message_id,person_id,operator_id,revision_id,session_id,rejected_at,hash,key_id) VALUES ('completion',$1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
            &[&self.operation_id, &self.account_id, &self.message_id, &self.person_id, &self.operator_revision_id,
                &self.revision_id, &self.session_id, &self.recorded_at, &&self.hash[..], &self.key_id],
        ).await?;
        Ok(())
    }
}

const REJECTED: &str = "SELECT transfer.id, rejection.account_id, rejection.operation_id, rejection.person_id,
    rejection.operator_id AS operator_revision_id, rejection.revision_id,
    rejection.session_id, rejection.message_id, rejection.rejected_at AS recorded_at, rejection.hash, rejection.key_id,
    confirmation.expires_at + interval '24 hours' AS expires_at,
    COALESCE(ROW(rejection.person_id,rejection.revision_id,rejection.operator_id,rejection.session_id)
        = ROW(transfer.person_id,transfer.revision_id,transfer.operator_id,transfer.session_id)
        AND challenge.purpose='mobile_binding',false) AS valid
    FROM identity.rejections rejection
    LEFT JOIN identity.confirmations confirmation ON confirmation.id=rejection.message_id
    LEFT JOIN identity.messages message ON message.id=rejection.message_id
    LEFT JOIN identity.challenges challenge ON challenge.id=message.challenge_id
    LEFT JOIN identity.transfers transfer ON transfer.id=challenge.transfer_id
    WHERE rejection.purpose='completion'";

pub(super) async fn find_rejection(
    client: &Client,
    keys: history::Keys<'_>,
    operation: operation::Id,
) -> Result<Option<Rejected>, Error> {
    let row = client
        .query_opt(
            &format!("{REJECTED} AND rejection.operation_id=$1"),
            &[&operation],
        )
        .await?;
    Ok(restore(client, keys, row.into_iter().collect())
        .await?
        .pop())
}

async fn restore(
    client: &Client,
    keys: history::Keys<'_>,
    rows: Vec<Row>,
) -> Result<Vec<Rejected>, Error> {
    let ids = rows
        .iter()
        .map(|row| {
            Ok([
                row.try_get::<_, person::revision::Id>("revision_id")?,
                row.try_get("operator_revision_id")?,
            ])
        })
        .collect::<Result<Vec<_>, tokio_postgres::Error>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let profiles = person::revision::fetch_many(client, keys, &ids).await?;
    rows.into_iter()
        .map(|row| Rejected::decode(row, &profiles))
        .collect()
}

pub(super) async fn rejections(
    client: &Client,
    keys: history::Keys<'_>,
) -> Result<Vec<Rejected>, Error> {
    let mut cursor = None::<operation::Id>;
    let mut result = Vec::new();
    loop {
        let records = verification::Stage::TransferCompletionsBatch.batch(BUDGET, async {
        let rows = client.query(
            &format!("{REJECTED} AND ($1::uuid IS NULL OR rejection.operation_id>$1) ORDER BY rejection.operation_id LIMIT 128"),
            &[&cursor],
        ).await?;
        restore(client, keys, rows).await
        }).await??;
        if records.is_empty() {
            break;
        }
        for record in records {
            cursor = Some(record.operation_id);
            result.push(record);
        }
    }
    Ok(result)
}

pub(super) async fn lock_confirmation(client: &Client, id: message::Id) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM identity.confirmations WHERE id=$1 FOR UPDATE",
            &[&id],
        )
        .await?;
    Ok(())
}

pub(super) struct Confirmation {
    id: message::Id,
    transfer_id: Id,
    person_id: person::Id,
    revision_id: person::revision::Id,
    operator_id: person::revision::Id,
    session_id: session::Id,
    established_at: DateTime<Utc>,
    mobile: Envelope,
    proof: message::proof::Proof,
    key_id: keys::Id,
    failures: i16,
    expires_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    superseded: bool,
    available: bool,
}

impl TryFrom<Row> for Confirmation {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(Self {
            id: row.try_get("id")?,
            transfer_id: row.try_get("transfer_id")?,
            person_id: row.try_get("person_id")?,
            revision_id: row.try_get("revision_id")?,
            operator_id: row.try_get("operator_id")?,
            session_id: row.try_get("session_id")?,
            established_at: row.try_get("established_at")?,
            mobile: row.try_get("mobile")?,
            proof: row.try_get("hash")?,
            key_id: row.try_get("key_id")?,
            failures: row.try_get("failures")?,
            expires_at: row.try_get("expires_at")?,
            created_at: row.try_get("created_at")?,
            superseded: row.try_get("superseded")?,
            available: row.try_get("available")?,
        })
    }
}

impl Confirmation {
    pub(super) fn transfer_id(&self) -> Id {
        self.transfer_id
    }
    pub(super) fn operator_id(&self) -> person::revision::Id {
        self.operator_id
    }
    pub(super) fn retained_until(&self) -> Result<DateTime<Utc>, Error> {
        self.expires_at
            .checked_add_signed(TimeDelta::hours(24))
            .ok_or(Error::Facts {
                rule: "a rejection retention deadline is representable",
            })
    }
    pub(super) fn deadline(&self) -> Result<DateTime<Utc>, Error> {
        let transfer = self
            .established_at
            .checked_add_signed(TimeDelta::minutes(10))
            .ok_or(Error::Facts {
                rule: "a transfer deadline is representable",
            })?;
        Ok(self.expires_at.min(transfer))
    }
    pub(super) fn qualify(
        &self,
        keys: &Keys,
        actor: &Actor,
        contact: &Target,
        input: &Input,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        if self.created_at > now
            || self.established_at > self.created_at
            || now < contact.observed_at()
        {
            return Err(Error::Facts {
                rule: "mobile completion verification follows its transfer and precedes trusted time",
            });
        }
        if self.person_id != contact.person_id()
            || self.revision_id != contact.revision_id()
            || self.session_id != actor.session_id()
            || !self.available
            || self.superseded
            || self.deadline()? <= now
            || self.failures >= 5
            || !self.mobile.matches(&keys.mobile, &input.mobile)?
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

pub(super) async fn find_confirmation(
    client: &Client,
    keys: history::Keys<'_>,
    message_id: message::Id,
    now: DateTime<Utc>,
) -> Result<Option<Confirmation>, Error> {
    let row = client.query_opt("SELECT confirmation.id, transfer.id AS transfer_id, transfer.person_id, transfer.revision_id, transfer.operator_id, transfer.session_id,
        transfer.created_at AS established_at, challenge.mobile, confirmation.hash, confirmation.key_id, confirmation.failures, confirmation.expires_at, message.created_at,
        EXISTS (SELECT 1 FROM identity.confirmations later JOIN identity.messages delivery ON delivery.id=later.id
            JOIN identity.challenges original ON original.id=delivery.challenge_id WHERE later.person_id=confirmation.person_id
                AND original.purpose='mobile_binding' AND (original.mobile).hash=(challenge.mobile).hash AND delivery.ordinal>message.ordinal) AS superseded,
        NOT EXISTS (SELECT 1 FROM identity.transfers later WHERE later.person_id=transfer.person_id AND later.created_at>transfer.created_at) AS available
        FROM identity.confirmations confirmation JOIN identity.messages message ON message.id=confirmation.id
        JOIN identity.challenges challenge ON challenge.id=message.challenge_id
        JOIN identity.transfers transfer ON transfer.id=challenge.transfer_id
        WHERE confirmation.id=$1 AND challenge.purpose='mobile_binding'", &[&message_id]).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let mut confirmation = Confirmation::try_from(row)?;
    if account::record::replacement(client, keys, confirmation.transfer_id, now)
        .await?
        .is_some()
    {
        confirmation.available = false;
    }
    Ok(Some(confirmation))
}

pub(crate) struct Snapshot {
    person_id: person::Id,
    source: Source,
}
enum Source {
    Transfer {
        created_at: DateTime<Utc>,
        latest: bool,
    },
    Completion {
        completed_at: DateTime<Utc>,
    },
}
impl Snapshot {
    pub(crate) fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub(crate) fn into_state(self, now: DateTime<Utc>) -> Result<State, Error> {
        match self.source {
            Source::Completion { completed_at } => Ok(State::Completed { completed_at }),
            Source::Transfer { created_at, latest } => {
                if created_at > now {
                    return Err(Error::Facts {
                        rule: "the mobile transfer precedes its decisive read",
                    });
                }
                let expires_at = created_at
                    .checked_add_signed(TimeDelta::minutes(10))
                    .ok_or(Error::Facts {
                        rule: "transfer deadline must be representable",
                    })?;
                if !latest || expires_at <= now {
                    Ok(State::Expired)
                } else {
                    Ok(State::Pending { expires_at })
                }
            }
        }
    }
}
pub(crate) async fn find_by_id(
    client: &Client,
    keys: history::Keys<'_>,
    id: Id,
    now: DateTime<Utc>,
) -> Result<Option<Snapshot>, Error> {
    if let Some(record) = account::record::replacement(client, keys, id, now).await? {
        return Ok(Some(Snapshot {
            person_id: record.person_id(),
            source: Source::Completion {
                completed_at: record.happened_at(),
            },
        }));
    }
    client.query_opt("SELECT transfer.person_id,transfer.created_at,NOT EXISTS(SELECT 1 FROM identity.transfers later WHERE later.person_id=transfer.person_id AND later.created_at>transfer.created_at) AS latest FROM identity.transfers transfer WHERE transfer.id=$1", &[&id]).await?.map(|row| {
        Ok(Snapshot { person_id: row.try_get("person_id")?, source: Source::Transfer { created_at: row.try_get("created_at")?, latest: row.try_get("latest")? } })
    }).transpose()
}
