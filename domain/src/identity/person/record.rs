//! The permanent Person record family. Origin, correction and cancellation records share one
//! object-local sequence behind the fixed [`CHANGES`] projection.

use std::{collections::HashSet, sync::Arc};

use chrono::{DateTime, Utc};
use platform::cryptography::envelope;
use tokio_postgres::{Client, GenericClient, Row};
use zeroize::Zeroizing;

use crate::{
    access::appointment,
    database::sealed::Sealed,
    history::{self, Error, Keys, Kind as _, Token},
    identity::{
        account, cancellation, correction,
        mobile::Mobile,
        operation,
        person::{self, revision, state::State},
    },
    keys::seal::Seal,
};

pub(crate) const CHANGES: &str = "identity.changes";
pub(crate) const ORIGINS: &str = "identity.origins";
pub(crate) const CORRECTIONS: &str = "identity.corrections";
pub(crate) const CANCELLATIONS: &str = "identity.cancellations";

pub(crate) struct Record {
    person_id: person::Id,
    sequence: i64,
    data: Data,
}

#[derive(Clone)]
pub(crate) struct Data {
    pub(super) operation_id: operation::Id,
    pub(super) happened_at: DateTime<Utc>,
    pub(super) event: Event,
}

#[derive(Clone)]
pub(crate) enum Event {
    Origin {
        account_id: account::Id,
        revision_id: revision::Id,
    },
    Correction {
        id: correction::Id,
        revision_id: revision::Id,
        before_id: revision::Id,
        operator_id: revision::Id,
        appointment_id: Option<appointment::Id>,
    },
    Cancellation {
        id: cancellation::Id,
        account_id: account::Id,
        revision_id: revision::Id,
        operator_id: revision::Id,
        mobile: Arc<Mobile>,
    },
}

impl Event {
    fn label(&self) -> &'static str {
        match self {
            Self::Origin { .. } => "origin",
            Self::Correction { .. } => "correction",
            Self::Cancellation { .. } => "cancellation",
        }
    }

    pub(crate) fn revision_id(&self) -> revision::Id {
        match self {
            Self::Origin { revision_id, .. }
            | Self::Correction { revision_id, .. }
            | Self::Cancellation { revision_id, .. } => *revision_id,
        }
    }
}

impl Record {
    pub(crate) fn person_id(&self) -> person::Id {
        self.person_id
    }

    pub(crate) fn sequence(&self) -> i64 {
        self.sequence
    }

    pub(crate) fn operation_id(&self) -> operation::Id {
        self.data.operation_id
    }

    pub(crate) fn happened_at(&self) -> DateTime<Utc> {
        self.data.happened_at
    }

    pub(crate) fn event(&self) -> &Event {
        &self.data.event
    }

    pub(super) fn matches(&self, state: &State) -> bool {
        self.person_id == state.id()
            && self.sequence == history::State::sequence(state)
            && self.data.happened_at == state.changed_at()
            && self.data.event.revision_id() == state.revision_id()
    }
}

impl history::Kind for Record {
    type Error = Error;
    const NAME: &'static str = "identity.person.record";
    const SELECT: &'static str = "SELECT record.person_id, record.sequence, record.operation_id, record.happened_at, record.kind, record.id, record.revision_id, record.before_id, record.operator_id, record.appointment_id, record.account_id, record.mobile, record.paired, record.seal, record.seal_key_id, record.seal_version FROM identity.changes record";

    fn decode(_: Token, row: &Row, keys: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let sequence: i64 = row.try_get("sequence").map_err(Error::decode)?;
        let happened_at: DateTime<Utc> = row.try_get("happened_at").map_err(Error::decode)?;
        if sequence < 1
            || happened_at > now
            || !row.try_get::<_, bool>("paired").map_err(Error::decode)?
        {
            return Err(Error::Integrity(
                "a person record retains its sequence, time and permanent source",
            ));
        }
        let event = match row.try_get::<_, &str>("kind").map_err(Error::decode)? {
            "origin" => {
                if sequence != 1 {
                    return Err(Error::Integrity(
                        "a person starts with exactly one origin record",
                    ));
                }
                Event::Origin {
                    account_id: row.try_get("account_id").map_err(Error::decode)?,
                    revision_id: row.try_get("revision_id").map_err(Error::decode)?,
                }
            }
            "correction" => {
                if sequence <= 1 {
                    return Err(Error::Integrity("an origin is the first person record"));
                }
                let revision_id: revision::Id =
                    row.try_get("revision_id").map_err(Error::decode)?;
                Event::Correction {
                    id: correction::Id::from_revision(revision_id),
                    revision_id,
                    before_id: row.try_get("before_id").map_err(Error::decode)?,
                    operator_id: row.try_get("operator_id").map_err(Error::decode)?,
                    appointment_id: row.try_get("appointment_id").map_err(Error::decode)?,
                }
            }
            "cancellation" => {
                if sequence <= 1 {
                    return Err(Error::Integrity("an origin is the first person record"));
                }
                let id: cancellation::Id = row.try_get("id").map_err(Error::decode)?;
                let mobile: Sealed = row.try_get("mobile").map_err(Error::decode)?;
                Event::Cancellation {
                    id,
                    account_id: row.try_get("account_id").map_err(Error::decode)?,
                    revision_id: row.try_get("revision_id").map_err(Error::decode)?,
                    operator_id: row.try_get("operator_id").map_err(Error::decode)?,
                    mobile: Arc::new(open_mobile(&mobile, keys.data, id)?),
                }
            }
            _ => {
                return Err(Error::Integrity(
                    "a person record has a declared event family",
                ));
            }
        };
        Ok(Self {
            person_id: row.try_get("person_id").map_err(Error::decode)?,
            sequence,
            data: Data {
                operation_id: row.try_get("operation_id").map_err(Error::decode)?,
                happened_at,
                event,
            },
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let mut fields = vec![
            self.person_id.as_bytes().to_vec(),
            self.sequence.to_be_bytes().to_vec(),
            self.data.operation_id.as_bytes().to_vec(),
            self.data
                .happened_at
                .timestamp_micros()
                .to_be_bytes()
                .to_vec(),
            self.data.event.label().as_bytes().to_vec(),
        ];
        match &self.data.event {
            Event::Origin {
                account_id,
                revision_id,
            } => fields.extend([
                revision_id.as_bytes().to_vec(),
                account_id.as_bytes().to_vec(),
            ]),
            Event::Correction {
                revision_id,
                before_id,
                operator_id,
                appointment_id,
                ..
            } => {
                fields.extend([
                    revision_id.as_bytes().to_vec(),
                    before_id.as_bytes().to_vec(),
                    operator_id.as_bytes().to_vec(),
                    appointment_id
                        .map(|id| id.as_bytes().to_vec())
                        .unwrap_or_default(),
                ]);
            }
            Event::Cancellation {
                id,
                account_id,
                revision_id,
                operator_id,
                mobile,
            } => {
                fields.extend([
                    id.as_bytes().to_vec(),
                    revision_id.as_bytes().to_vec(),
                    account_id.as_bytes().to_vec(),
                    operator_id.as_bytes().to_vec(),
                    mobile.as_bytes().to_vec(),
                ]);
            }
        }
        Zeroizing::new(fields)
    }
}

struct Binding(cancellation::Id);

impl envelope::Binding for Binding {
    fn domain(&self) -> &[u8] {
        b"nucleus/identity/envelope/v1"
    }

    fn identity(&self) -> &[u8] {
        self.0.as_bytes()
    }

    fn field(&self) -> &[u8] {
        b"cancellation.mobile"
    }
}

struct Plaintext<'a> {
    binding: Binding,
    mobile: &'a Mobile,
}

impl envelope::Binding for Plaintext<'_> {
    fn domain(&self) -> &[u8] {
        self.binding.domain()
    }

    fn identity(&self) -> &[u8] {
        self.binding.identity()
    }

    fn field(&self) -> &[u8] {
        self.binding.field()
    }
}

impl envelope::Input for Plaintext<'_> {
    fn plaintext(&self) -> &[u8] {
        self.mobile.as_bytes()
    }
}

fn open_mobile(
    value: &Sealed,
    keys: &crate::identity::keys::encryption::Keys,
    id: cancellation::Id,
) -> Result<Mobile, Error> {
    let plaintext = keys.open(&value.0, &Binding(id))?;
    let mobile = std::str::from_utf8(&plaintext)?.parse().map_err(
        |_: crate::identity::mobile::ParseError| {
            Error::Integrity("a cancellation retains a canonical mobile")
        },
    )?;
    Ok(mobile)
}

impl history::Record for Record {
    const OBJECT: &'static str = "person_id";
    type Object = person::Id;
    type Data = Data;

    fn build(_: Token, person_id: person::Id, sequence: i64, data: Data) -> Self {
        Self {
            person_id,
            sequence,
            data,
        }
    }

    fn object(&self) -> person::Id {
        self.person_id
    }

    fn sequence(&self) -> i64 {
        self.sequence
    }

    fn operation_id(&self) -> operation::Id {
        self.data.operation_id
    }

    fn follows(&self, previous: &Self) -> bool {
        self.person_id == previous.person_id
            && self.data.happened_at >= previous.data.happened_at
            && match &self.data.event {
                Event::Origin { .. } => false,
                Event::Correction { before_id, .. } => {
                    *before_id == previous.data.event.revision_id()
                }
                Event::Cancellation { revision_id, .. } => {
                    *revision_id == previous.data.event.revision_id()
                }
            }
    }

    async fn insert(
        &self,
        _: Token,
        client: &Client,
        keys: Keys<'_>,
        seal: &Seal,
    ) -> Result<(), Error> {
        match &self.data.event {
            Event::Origin {
                account_id,
                revision_id,
            } => {
                client.execute(
                    "INSERT INTO identity.origins (person_id, account_id, revision_id, operation_id, created_at, sequence, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
                    &[&self.person_id, account_id, revision_id, &self.data.operation_id,
                      &self.data.happened_at, &self.sequence, &seal.tag(), &seal.key_id(),
                      &seal.version()],
                ).await?;
            }
            Event::Correction {
                id,
                revision_id,
                before_id,
                operator_id,
                appointment_id,
            } => {
                client.execute(
                    "INSERT INTO identity.corrections (id, person_id, operator_id, before_id, corrected_at, sequence, operation_id, appointment_id, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
                    &[&id.revision_id(), &self.person_id, operator_id, before_id,
                      &self.data.happened_at, &self.sequence, &self.data.operation_id,
                      appointment_id, &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
                let _ = revision_id;
            }
            Event::Cancellation {
                id,
                account_id,
                revision_id,
                operator_id,
                mobile,
            } => {
                let mobile = Sealed(keys.data.seal(&Plaintext {
                    binding: Binding(*id),
                    mobile,
                })?);
                client.execute(
                    "INSERT INTO identity.cancellations (id, account_id, person_id, revision_id, operator_id, mobile, cancelled_at, sequence, operation_id, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
                    &[id, account_id, &self.person_id, revision_id, operator_id, &mobile,
                      &self.data.happened_at, &self.sequence, &self.data.operation_id,
                      &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
            }
        }
        Ok(())
    }
}

pub(crate) async fn origin(
    client: &impl GenericClient,
    keys: Keys<'_>,
    person: person::Id,
    now: DateTime<Utc>,
) -> Result<Option<history::Verified<Record>>, Error> {
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.kind='origin' AND record.person_id=$1",
                Record::SELECT
            ),
            &[&person],
        )
        .await?;
    row.as_ref()
        .map(|row| history::record(row, keys, now))
        .transpose()
}

/// Move only registration origins in the disposable SMS fixture's existing transaction.
#[cfg(any(test, feature = "test-support"))]
pub(crate) async fn age(
    client: &Client,
    keys: Keys<'_>,
    now: DateTime<Utc>,
    elapsed: chrono::TimeDelta,
    persons: &[person::Id],
) -> Result<Vec<history::Verified<State>>, Error> {
    use history::{Seed, State as _};

    let selected = persons.iter().copied().collect::<HashSet<_>>();
    let states = history::inspect::<State>(client, keys, now).await?;
    let mut dated_states = Vec::new();
    for state in states {
        if !selected.contains(&state.id()) {
            continue;
        }
        let original = origin(client, keys, state.id(), now)
            .await?
            .ok_or(Error::Integrity(
                "a registration fixture retains its person origin",
            ))?;
        let mut data = original.data.clone();
        data.happened_at -= elapsed;
        let dated = Seed::record::<Record>(state.id(), 1, data, keys)?;
        client.execute(
            "UPDATE identity.origins SET created_at=$2,seal=$3,seal_key_id=$4,seal_version=$5 WHERE person_id=$1",
            &[&state.id(), &dated.happened_at(), &dated.seal().tag(), &dated.seal().key_id(), &dated.seal().version()],
        ).await?;
        if state.sequence() == 1 {
            dated_states.push(super::state::age(client, keys, &state, elapsed).await?);
        }
    }
    Ok(dated_states)
}

/// A keyless structural check for the first revision's permanent origin. Seals are verified by
/// the startup and authenticated loaders; this only binds the revision to its origin record.
pub(crate) async fn verify_origin(
    client: &impl GenericClient,
    person: person::Id,
    revision: revision::Id,
) -> Result<(), Error> {
    let sourced: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM identity.changes record WHERE record.kind='origin'
                AND record.person_id=$1 AND record.revision_id=$2 AND record.paired)",
            &[&person, &revision],
        )
        .await?
        .try_get(0)?;
    if !sourced {
        return Err(Error::Integrity(
            "the first real-name version retains its permanent origin",
        ));
    }
    Ok(())
}

pub(crate) async fn find_correction(
    client: &impl GenericClient,
    keys: Keys<'_>,
    operation: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<history::Verified<Record>>, Error> {
    find(client, keys, "correction", operation, now).await
}

pub(crate) async fn find_cancellation(
    client: &impl GenericClient,
    keys: Keys<'_>,
    operation: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<history::Verified<Record>>, Error> {
    find(client, keys, "cancellation", operation, now).await
}

async fn find(
    client: &impl GenericClient,
    keys: Keys<'_>,
    kind: &str,
    operation: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<history::Verified<Record>>, Error> {
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.kind=$1 AND record.operation_id=$2",
                Record::SELECT
            ),
            &[&kind, &operation],
        )
        .await?;
    row.as_ref()
        .map(|row| history::record(row, keys, now))
        .transpose()
}

pub(crate) async fn batch(
    client: &impl GenericClient,
    keys: Keys<'_>,
    kind: &'static str,
    stage: crate::database::verification::Stage,
    cursor: Option<(person::Id, i64)>,
    now: DateTime<Utc>,
) -> Result<Vec<history::Verified<Record>>, Error> {
    let rows = stage
            .batch(
                std::time::Duration::from_secs(10),
                client.query(
                    &format!(
                        "{} WHERE record.kind=$1 AND ($2::uuid IS NULL OR (record.person_id, record.sequence) > ($2::uuid, $3::bigint)) ORDER BY record.person_id, record.sequence LIMIT 128",
                        Record::SELECT
                    ),
                    &[&kind, &cursor.map(|(person, _)| person), &cursor.map(|(_, sequence)| sequence)],
                ),
            )
            .await??;
    rows.iter()
        .map(|row| history::record(row, keys, now))
        .collect()
}

pub(crate) async fn cancellations_for_persons(
    client: &impl GenericClient,
    keys: Keys<'_>,
    persons: &[person::Id],
    now: DateTime<Utc>,
) -> Result<Vec<history::Verified<Record>>, Error> {
    if persons.is_empty() {
        return Ok(Vec::new());
    }
    let rows = client
        .query(
            &format!(
                "{} WHERE record.kind='cancellation' AND record.person_id=ANY($1) ORDER BY record.person_id, record.sequence",
                Record::SELECT
            ),
            &[&persons],
        )
        .await?;
    rows.iter()
        .map(|row| history::record(row, keys, now))
        .collect()
}

/// Revision ids whose permanent origin or correction source is authenticated in this query.
pub(crate) async fn sourced(
    client: &impl GenericClient,
    pairs: &[(revision::Id, person::Id)],
) -> Result<HashSet<revision::Id>, Error> {
    if pairs.is_empty() {
        return Ok(HashSet::new());
    }
    let revisions = pairs.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let persons = pairs.iter().map(|(_, person)| *person).collect::<Vec<_>>();
    let rows = client
        .query(
            "WITH expected AS (SELECT * FROM unnest($1::uuid[],$2::uuid[]) AS value(revision_id,person_id))
             SELECT DISTINCT record.revision_id
             FROM expected JOIN identity.changes record
                 ON record.revision_id=expected.revision_id AND record.person_id=expected.person_id
             WHERE record.paired AND record.kind IN ('origin', 'correction')",
            &[&revisions, &persons],
        )
        .await?;
    rows.iter()
        .map(|row| row.try_get(0).map_err(Error::decode))
        .collect()
}
