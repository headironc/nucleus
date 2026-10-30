//! The institutional relationship: current authority, immutable appointments and its tombstone.

use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use zeroize::Zeroizing;

use crate::{
    access::{appointment, role},
    database::verification,
    history::{
        self, Change, Draft, Error, Keys, Kind as _, State as HistoryState, Token, Verified,
    },
    identity::{
        account, cancellation, operation,
        person::{self, revision},
    },
    keys::seal::Seal,
};

pub(crate) const HOLDERS: &str = "access.holders";
pub(crate) const APPOINTMENTS: &str = "access.appointments";
pub(crate) const PROJECTIONS: &str = "access.projections";

pub(crate) async fn lock(client: &Client, id: account::Id) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM access.holders WHERE id=$1 FOR UPDATE",
            &[&id],
        )
        .await?;
    Ok(())
}

pub(in crate::access) async fn lock_role_holders(
    client: &Client,
    role_id: role::Id,
    account_id: account::Id,
) -> Result<(), Error> {
    // This locator selects locks only. Authorization follows every possible wait.
    client
        .query_one(
            "SELECT access.lock_role_holders($1, (SELECT role_id FROM access.holders WHERE id=$2))",
            &[&role_id, &account_id],
        )
        .await?;
    Ok(())
}

pub(in crate::access) async fn replacements(client: &Client) -> Result<i64, Error> {
    client
        .query_one(
            "SELECT count(*) FROM access.appointments WHERE source IN ('session','system')",
            &[],
        )
        .await?
        .try_get(0)
        .map_err(Error::decode)
}

pub(in crate::access) async fn counts(
    client: &Client,
    person_id: Option<person::Id>,
    operation_id: Option<operation::Id>,
) -> Result<(i64, i64), Error> {
    let row = client.query_one(
        "SELECT count(*) AS total,count(*) FILTER (WHERE ($1::uuid IS NULL OR person_id=$1) AND ($2::uuid IS NULL OR operation_id=$2)) AS selected FROM access.appointments",
        &[&person_id, &operation_id],
    ).await?;
    Ok((
        row.try_get("total").map_err(Error::decode)?,
        row.try_get("selected").map_err(Error::decode)?,
    ))
}

pub(in crate::access) async fn verify_cancellations(
    client: &Client,
    keys: Keys<'_>,
    states: &[Verified<State>],
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let mut tombstones: HashMap<_, _> = states
        .iter()
        .filter(|state| !state.active())
        .map(|state| (*state.id.as_bytes(), state))
        .collect();
    let rows = client
        .query(
            &format!("{} WHERE record.source='cancellation'", Record::SELECT),
            &[],
        )
        .await?;
    for row in &rows {
        let record = history::record::<Record>(row, keys, now)?;
        let state = tombstones
            .remove(record.holder_id.as_bytes())
            .ok_or(Error::Integrity(
                "a cancellation projection belongs to one retained holder tombstone",
            ))?;
        if record.sequence != state.sequence || !state.has_head(&record) {
            return Err(Error::Integrity(
                "a cancellation projection is its holder's authenticated history head",
            ));
        }
    }
    if !tombstones.is_empty() {
        return Err(Error::Integrity(
            "every holder tombstone retains its cancellation projection",
        ));
    }
    Ok(())
}

pub(in crate::access) async fn initialization(
    client: &Client,
    keys: Keys<'_>,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<Verified<Record>>, Error> {
    client
        .query_opt(
            &format!(
                "{} WHERE record.operation_id=$1 AND record.source='initialization'",
                Record::SELECT
            ),
            &[&operation_id],
        )
        .await?
        .map(|row| history::record(&row, keys, now))
        .transpose()
}

pub(in crate::access) async fn initializations(client: &Client) -> Result<i64, Error> {
    client
        .query_one(
            "SELECT count(*) FROM access.appointments WHERE source='initialization'",
            &[],
        )
        .await?
        .try_get(0)
        .map_err(Error::decode)
}

pub(in crate::access) async fn verify_links(
    client: &Client,
    keys: Keys<'_>,
    accounts: &[Verified<account::state::State>],
    holders: &[Verified<State>],
) -> Result<(), crate::access::Error> {
    let mut remaining: HashMap<_, _> = accounts.iter().map(|state| (state.id(), state)).collect();
    for holder in holders {
        let account =
            remaining
                .remove(&holder.account_id())
                .ok_or(crate::access::Error::Facts {
                    rule: "each holder retains its permanent account",
                })?;
        if account.person_id() != holder.person_id()
            || account.cancellation_id() != holder.cancellation_id
        {
            return Err(crate::access::Error::Facts {
                rule: "the account and holder retain the same person and cancellation",
            });
        }
    }
    if !remaining.is_empty() {
        return Err(crate::access::Error::Facts {
            rule: "each permanent account retains its holder",
        });
    }
    let mut cursor: Option<appointment::Id> = None;
    loop {
        cursor = verification::Stage::AccessRelations.batch(Duration::from_secs(10), async {
        let rows = client.query(
            "SELECT id,person_id,revision_id,operator_id FROM access.appointments WHERE $1::uuid IS NULL OR id>$1 ORDER BY id LIMIT 128",
            &[&cursor],
        ).await?;
        let ids = rows
            .iter()
            .map(|row| {
                Ok([
                    row.try_get::<_, revision::Id>("revision_id")?,
                    row.try_get("operator_id")?,
                ])
            })
            .collect::<Result<Vec<_>, tokio_postgres::Error>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let profiles = revision::fetch_many(client, keys, &ids).await?;
        let mut next = None;
        for row in rows {
            let person_id = row.try_get("person_id")?;
            let revision_id = row.try_get("revision_id")?;
            if profiles
                .get(&revision_id)
                .is_none_or(|profile| profile.person_id() != person_id)
            {
                return Err(crate::access::Error::Facts {
                    rule: "an appointment retains its person's fixed identity",
                });
            }
            next = Some(row.try_get("id")?);
        }
        Ok::<_, crate::access::Error>(next)
        }).await??;
        if cursor.is_none() {
            break;
        }
    }
    Ok(())
}

/// A joined authorization delays authentication until its session has been confirmed.
pub(in crate::access) async fn fetch(
    client: &Client,
    id: account::Id,
) -> Result<Option<Row>, Error> {
    Ok(client
        .query_opt(&format!("{} WHERE state.id = $1", State::SELECT), &[&id])
        .await?)
}

/// The original creation is permanent even when R2's session and handoff have expired.
pub(crate) async fn creation(
    client: &impl tokio_postgres::GenericClient,
    keys: Keys<'_>,
    id: account::Id,
    now: DateTime<Utc>,
) -> Result<Option<Verified<Record>>, Error> {
    client
        .query_opt(
            &format!(
                "{} WHERE record.holder_id=$1 AND record.sequence=1",
                Record::SELECT
            ),
            &[&id],
        )
        .await?
        .map(|row| history::record(&row, keys, now))
        .transpose()
}

#[cfg(test)]
async fn appointment(
    client: &impl tokio_postgres::GenericClient,
    keys: Keys<'_>,
    id: appointment::Id,
    now: DateTime<Utc>,
) -> Result<Verified<Record>, Error> {
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.assignment_id=$1 AND record.source<>'cancellation'",
                Record::SELECT
            ),
            &[&id],
        )
        .await?
        .ok_or(Error::Integrity(
            "a fixed appointment reference retains its record",
        ))?;
    history::record(&row, keys, now)
}

pub(crate) async fn appointments(
    client: &impl tokio_postgres::GenericClient,
    keys: Keys<'_>,
    ids: &[appointment::Id],
    now: DateTime<Utc>,
) -> Result<Vec<Verified<Record>>, Error> {
    client
        .query(
            &format!(
                "{} WHERE record.assignment_id=ANY($1::uuid[]) AND record.source<>'cancellation'",
                Record::SELECT
            ),
            &[&ids],
        )
        .await?
        .iter()
        .map(|row| history::record(row, keys, now))
        .collect()
}

pub(crate) async fn created(
    client: &Client,
    keys: Keys<'_>,
    anchors: &history::Anchors,
    id: account::Id,
    operation: operation::Id,
    source: Source,
    now: DateTime<Utc>,
) -> Result<bool, Error> {
    let Some(record) = creation(client, keys, id, now).await? else {
        if fetch(client, id).await?.is_some() {
            return Err(Error::Integrity(
                "a retained holder has its creation record",
            ));
        }
        return Ok(false);
    };
    if record.holder_id != id
        || record.operation_id != operation
        || !record
            .assignment()
            .is_some_and(|assignment| assignment.source == source)
    {
        return Err(Error::Integrity(
            "the created holder retains its original operation and source",
        ));
    }
    let assignment = record.assignment().ok_or(Error::Integrity(
        "the first holder record is an appointment",
    ))?;
    revision::verify_initial(client, record.person_id(), assignment.revision_id).await?;
    anchors.confirm::<State>(&record)?;
    Ok(true)
}

pub(crate) async fn confirm_cancellation(
    client: &Client,
    keys: Keys<'_>,
    anchors: &history::Anchors,
    id: cancellation::Id,
    operation: operation::Id,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let row = client
        .query_opt(
            &format!("{} WHERE record.cancellation_id = $1", Record::SELECT),
            &[&id],
        )
        .await?
        .ok_or(Error::Integrity(
            "a cancellation retains its holder projection",
        ))?;
    let record = history::record::<Record>(&row, keys, now)?;
    if record.cancellation_id() != Some(id) || record.operation_id != operation {
        return Err(Error::Integrity(
            "the holder projection belongs to the original cancellation",
        ));
    }
    anchors.confirm::<State>(&record)
}

/// The account ID remains the object's identity after the account itself is removed.
#[derive(Clone)]
pub(crate) struct State {
    id: account::Id,
    person_id: person::Id,
    role_id: role::Id,
    assignment_id: appointment::Id,
    cancellation_id: Option<cancellation::Id>,
    changed_at: DateTime<Utc>,
    sequence: i64,
    version: i64,
}

pub(crate) struct Data {
    id: account::Id,
    person_id: person::Id,
    role_id: role::Id,
    assignment_id: appointment::Id,
    cancellation_id: Option<cancellation::Id>,
    changed_at: DateTime<Utc>,
}

impl State {
    pub(crate) fn account_id(&self) -> account::Id {
        self.id
    }
    pub(crate) fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub(crate) fn role_id(&self) -> role::Id {
        self.role_id
    }
    pub(crate) fn assignment_id(&self) -> appointment::Id {
        self.assignment_id
    }
    pub(crate) fn active(&self) -> bool {
        self.cancellation_id.is_none()
    }
}

impl history::Kind for State {
    type Error = history::Error;
    const NAME: &'static str = "access.holder";
    const SELECT: &'static str = "SELECT state.id, state.person_id, state.role_id, state.assignment_id, state.cancellation_id, state.changed_at, state.sequence, state.version, state.seal, state.seal_key_id, state.seal_version FROM access.holders state";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let changed_at = row.try_get("changed_at").map_err(Error::decode)?;
        if changed_at > now {
            return Err(Error::Integrity("a holder state is not from the future"));
        }
        Ok(Self {
            id: row.try_get("id").map_err(Error::decode)?,
            person_id: row.try_get("person_id").map_err(Error::decode)?,
            role_id: row.try_get("role_id").map_err(Error::decode)?,
            assignment_id: row.try_get("assignment_id").map_err(Error::decode)?,
            cancellation_id: row.try_get("cancellation_id").map_err(Error::decode)?,
            changed_at,
            sequence: row.try_get("sequence").map_err(Error::decode)?,
            version: row.try_get("version").map_err(Error::decode)?,
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        Zeroizing::new(vec![
            self.id.as_bytes().to_vec(),
            self.person_id.as_bytes().to_vec(),
            self.role_id.as_bytes().to_vec(),
            self.assignment_id.as_bytes().to_vec(),
            self.cancellation_id
                .map(|id| id.as_bytes().to_vec())
                .unwrap_or_default(),
            self.changed_at.timestamp_micros().to_string().into_bytes(),
            self.sequence.to_string().into_bytes(),
            self.version.to_string().into_bytes(),
        ])
    }
}

impl HistoryState for State {
    type Id = account::Id;
    type Data = Data;
    type Record = Record;

    fn build(_: Token, data: Data, sequence: i64, version: i64) -> Self {
        Self {
            id: data.id,
            person_id: data.person_id,
            role_id: data.role_id,
            assignment_id: data.assignment_id,
            cancellation_id: data.cancellation_id,
            changed_at: data.changed_at,
            sequence,
            version,
        }
    }

    fn id(&self) -> account::Id {
        self.id
    }
    fn bytes(id: account::Id) -> [u8; 16] {
        *id.as_bytes()
    }
    fn sequence(&self) -> i64 {
        self.sequence
    }
    fn version(&self) -> i64 {
        self.version
    }
    fn has_head(&self, record: &Record) -> bool {
        record.person_id == self.person_id
            && record.role_id == self.role_id
            && record.assignment_id == self.assignment_id
            && record.happened_at == self.changed_at
            && record.cancellation_id() == self.cancellation_id
    }

    async fn write(
        &self,
        _: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> Result<u64, Error> {
        Ok(match original {
            None => client.execute(
                "INSERT INTO access.holders (id,person_id,role_id,assignment_id,cancellation_id,changed_at,sequence,version,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
                &[&self.id, &self.person_id, &self.role_id, &self.assignment_id, &self.cancellation_id, &self.changed_at, &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version()],
            ).await?,
            Some(original) => client.execute(
                "UPDATE access.holders SET role_id=$3,assignment_id=$4,cancellation_id=$5,changed_at=$6,sequence=$7,version=$8,seal=$9,seal_key_id=$10,seal_version=$11 WHERE id=$1 AND person_id=$2 AND version=$12 AND seal=$13 AND seal_key_id=$14 AND seal_version=$15",
                &[&self.id, &self.person_id, &self.role_id, &self.assignment_id, &self.cancellation_id, &self.changed_at, &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version(), &original.version(), &original.seal().tag(), &original.seal().key_id(), &original.seal().version()],
            ).await?,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Initialization,
    Registration,
    Session,
    System,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Initialization => "initialization",
            Self::Registration => "registration",
            Self::Session => "session",
            Self::System => "system",
        }
    }

    fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "initialization" => Ok(Self::Initialization),
            "registration" => Ok(Self::Registration),
            "session" => Ok(Self::Session),
            "system" => Ok(Self::System),
            _ => Err(Error::Integrity(
                "an appointment retains its declared source",
            )),
        }
    }

    fn first(self) -> bool {
        matches!(self, Self::Initialization | Self::Registration)
    }
}

pub(crate) struct Assignment {
    pub(crate) id: appointment::Id,
    pub(crate) previous_id: Option<appointment::Id>,
    pub(crate) change_id: role::change::Id,
    pub(crate) operator_id: revision::Id,
    pub(crate) revision_id: revision::Id,
    pub(crate) source: Source,
}

enum Event {
    Appointment(Assignment),
    Cancellation(cancellation::Id),
}

pub(crate) struct Record {
    holder_id: account::Id,
    person_id: person::Id,
    role_id: role::Id,
    assignment_id: appointment::Id,
    sequence: i64,
    happened_at: DateTime<Utc>,
    operation_id: operation::Id,
    event: Event,
}

pub(crate) struct RecordData {
    person_id: person::Id,
    role_id: role::Id,
    assignment_id: appointment::Id,
    happened_at: DateTime<Utc>,
    operation_id: operation::Id,
    event: Event,
}

impl Record {
    pub(crate) fn holder_id(&self) -> account::Id {
        self.holder_id
    }
    pub(crate) fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub(crate) fn role_id(&self) -> role::Id {
        self.role_id
    }
    pub(crate) fn assigned_at(&self) -> DateTime<Utc> {
        self.happened_at
    }
    pub(crate) fn assignment(&self) -> Option<&Assignment> {
        match &self.event {
            Event::Appointment(assignment) => Some(assignment),
            Event::Cancellation(_) => None,
        }
    }

    fn cancellation_id(&self) -> Option<cancellation::Id> {
        match self.event {
            Event::Appointment(_) => None,
            Event::Cancellation(id) => Some(id),
        }
    }
}

impl history::Kind for Record {
    type Error = history::Error;
    const NAME: &'static str = "access.holding";
    const SELECT: &'static str = r"SELECT record.holder_id,record.person_id,record.role_id,record.assignment_id,record.sequence,record.happened_at,record.operation_id,record.source,record.previous_id,record.change_id,record.operator_id,record.revision_id,record.cancellation_id,record.paired,record.seal,record.seal_key_id,record.seal_version
        FROM (
            SELECT appointment.holder_id,appointment.person_id,appointment.role_id,appointment.id AS assignment_id,appointment.sequence,appointment.assigned_at AS happened_at,appointment.operation_id,appointment.source,appointment.previous_id,appointment.change_id,appointment.operator_id,appointment.revision_id,NULL::uuid AS cancellation_id,
                CASE WHEN appointment.source IN ('initialization','registration') THEN EXISTS (
                    SELECT 1 FROM identity.changes origin WHERE origin.kind='origin' AND origin.person_id=appointment.person_id
                        AND origin.transaction_id=appointment.transaction_id
                ) ELSE true END AS paired,appointment.seal,appointment.seal_key_id,appointment.seal_version
            FROM access.appointments appointment
            UNION ALL
            SELECT projection.holder_id,projection.person_id,projection.role_id,projection.assignment_id,projection.sequence,projection.projected_at,projection.operation_id,'cancellation',NULL::uuid,NULL::uuid,NULL::uuid,NULL::uuid,projection.id,
                EXISTS (SELECT 1 FROM identity.changes cause WHERE cause.kind='cancellation' AND cause.id=projection.id AND cause.account_id=projection.holder_id AND cause.person_id=projection.person_id AND cause.happened_at=projection.projected_at AND cause.operation_id=projection.operation_id AND cause.transaction_id=projection.transaction_id),
                projection.seal,projection.seal_key_id,projection.seal_version
            FROM access.projections projection
        ) record";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let sequence = row.try_get("sequence").map_err(Error::decode)?;
        let happened_at = row.try_get("happened_at").map_err(Error::decode)?;
        if sequence < 1
            || happened_at > now
            || !row.try_get::<_, bool>("paired").map_err(Error::decode)?
        {
            return Err(Error::Integrity(
                "a holder record has a positive sequence, non-future time and retained cause",
            ));
        }
        let assignment_id = row.try_get("assignment_id").map_err(Error::decode)?;
        let source: String = row.try_get("source").map_err(Error::decode)?;
        let event = if source == "cancellation" {
            if sequence == 1 {
                return Err(Error::Integrity("a holder starts with an appointment"));
            }
            Event::Cancellation(row.try_get("cancellation_id").map_err(Error::decode)?)
        } else {
            let source = Source::parse(&source)?;
            let previous_id: Option<appointment::Id> =
                row.try_get("previous_id").map_err(Error::decode)?;
            let operator_id = row.try_get("operator_id").map_err(Error::decode)?;
            let revision_id = row.try_get("revision_id").map_err(Error::decode)?;
            if source.first() != (sequence == 1)
                || source.first() != previous_id.is_none()
                || (source.first() && operator_id != revision_id)
            {
                return Err(Error::Integrity(
                    "an appointment retains its source, predecessor and fixed first identity",
                ));
            }
            Event::Appointment(Assignment {
                id: assignment_id,
                previous_id,
                change_id: row.try_get("change_id").map_err(Error::decode)?,
                operator_id,
                revision_id,
                source,
            })
        };
        Ok(Self {
            holder_id: row.try_get("holder_id").map_err(Error::decode)?,
            person_id: row.try_get("person_id").map_err(Error::decode)?,
            role_id: row.try_get("role_id").map_err(Error::decode)?,
            assignment_id,
            sequence,
            happened_at,
            operation_id: row.try_get("operation_id").map_err(Error::decode)?,
            event,
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let mut fields = vec![
            self.holder_id.as_bytes().to_vec(),
            self.person_id.as_bytes().to_vec(),
            self.role_id.as_bytes().to_vec(),
            self.assignment_id.as_bytes().to_vec(),
            self.sequence.to_string().into_bytes(),
            self.happened_at.timestamp_micros().to_string().into_bytes(),
            self.operation_id.as_bytes().to_vec(),
        ];
        match &self.event {
            Event::Appointment(assignment) => fields.extend([
                assignment.source.label().as_bytes().to_vec(),
                assignment
                    .previous_id
                    .map(|id| id.as_bytes().to_vec())
                    .unwrap_or_default(),
                assignment.change_id.as_bytes().to_vec(),
                assignment.operator_id.as_bytes().to_vec(),
                assignment.revision_id.as_bytes().to_vec(),
            ]),
            Event::Cancellation(id) => {
                fields.extend([b"cancellation".to_vec(), id.as_bytes().to_vec()]);
            }
        }
        Zeroizing::new(fields)
    }
}

impl history::Record for Record {
    const OBJECT: &'static str = "holder_id";
    type Object = account::Id;
    type Data = RecordData;

    fn build(_: Token, holder_id: account::Id, sequence: i64, data: RecordData) -> Self {
        Self {
            holder_id,
            person_id: data.person_id,
            role_id: data.role_id,
            assignment_id: data.assignment_id,
            sequence,
            happened_at: data.happened_at,
            operation_id: data.operation_id,
            event: data.event,
        }
    }

    fn object(&self) -> account::Id {
        self.holder_id
    }
    fn sequence(&self) -> i64 {
        self.sequence
    }
    fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
    fn follows(&self, previous: &Self) -> bool {
        if !matches!(previous.event, Event::Appointment(_))
            || previous.person_id != self.person_id
            || self.happened_at < previous.happened_at
        {
            return false;
        }
        match &self.event {
            Event::Appointment(assignment) => {
                assignment.previous_id == Some(previous.assignment_id)
                    && self.role_id != previous.role_id
            }
            Event::Cancellation(_) => {
                self.assignment_id == previous.assignment_id && self.role_id == previous.role_id
            }
        }
    }

    async fn insert(
        &self,
        _: Token,
        client: &Client,
        _: Keys<'_>,
        seal: &Seal,
    ) -> Result<(), Error> {
        match &self.event {
            Event::Appointment(assignment) => {
                client.execute(
                    "INSERT INTO access.appointments (id,holder_id,person_id,role_id,previous_id,change_id,operator_id,revision_id,source,assigned_at,sequence,operation_id,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
                    &[&assignment.id, &self.holder_id, &self.person_id, &self.role_id, &assignment.previous_id, &assignment.change_id, &assignment.operator_id, &assignment.revision_id, &assignment.source.label(), &self.happened_at, &self.sequence, &self.operation_id, &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
            }
            Event::Cancellation(id) => {
                client.execute(
                    "INSERT INTO access.projections (id,holder_id,person_id,role_id,assignment_id,projected_at,sequence,operation_id,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
                    &[&id, &self.holder_id, &self.person_id, &self.role_id, &self.assignment_id, &self.happened_at, &self.sequence, &self.operation_id, &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
            }
        }
        Ok(())
    }
}

pub(crate) fn first(
    identity: &account::Creation,
    role: &role::Role,
    assignment: Assignment,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    let expected = match assignment.source {
        Source::Initialization => role::Kind::Full,
        Source::Registration => role::Kind::Default,
        Source::Session | Source::System => {
            return Err(Error::Integrity("only account creation makes a new holder"));
        }
    };
    if role.kind() != expected
        || assignment.previous_id.is_some()
        || assignment.change_id != role.change_id()
        || assignment.operator_id != identity.revision_id
        || assignment.revision_id != identity.revision_id
    {
        return Err(Error::Integrity(
            "a first appointment binds its system role and the new person's first revision",
        ));
    }
    let draft = Draft::create(Data {
        id: identity.account_id,
        person_id: identity.person_id,
        role_id: role.id(),
        assignment_id: assignment.id,
        cancellation_id: None,
        changed_at: now,
    });
    Ok(Change::new(
        draft,
        RecordData {
            person_id: identity.person_id,
            role_id: role.id(),
            assignment_id: assignment.id,
            happened_at: now,
            operation_id,
            event: Event::Appointment(assignment),
        },
    ))
}

pub(crate) fn replace(
    original: &Verified<State>,
    role: &role::Role,
    assignment: Assignment,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if !original.active()
        || assignment.source.first()
        || assignment.previous_id != Some(original.assignment_id)
        || assignment.change_id != role.change_id()
        || role.id() == original.role_id
        || now < original.changed_at
    {
        return Err(Error::Integrity(
            "replacement advances a live holder from its exact appointment",
        ));
    }
    let draft = Draft::advance(
        original,
        Data {
            id: original.id,
            person_id: original.person_id,
            role_id: role.id(),
            assignment_id: assignment.id,
            cancellation_id: None,
            changed_at: now,
        },
    )?;
    Ok(Change::new(
        draft,
        RecordData {
            person_id: original.person_id,
            role_id: role.id(),
            assignment_id: assignment.id,
            happened_at: now,
            operation_id,
            event: Event::Appointment(assignment),
        },
    ))
}

pub(crate) fn cancel(
    original: &Verified<State>,
    id: cancellation::Id,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if !original.active() || now < original.changed_at {
        return Err(Error::Integrity("cancellation advances a live holder"));
    }
    let draft = Draft::advance(
        original,
        Data {
            id: original.id,
            person_id: original.person_id,
            role_id: original.role_id,
            assignment_id: original.assignment_id,
            cancellation_id: Some(id),
            changed_at: now,
        },
    )?;
    Ok(Change::new(
        draft,
        RecordData {
            person_id: original.person_id,
            role_id: original.role_id,
            assignment_id: original.assignment_id,
            happened_at: now,
            operation_id,
            event: Event::Cancellation(id),
        },
    ))
}

/// Rebuild disposable historical fixtures; the caller commits before installing these anchors.
#[cfg(any(test, feature = "test-support"))]
pub(crate) async fn age_registrations(
    client: &Client,
    keys: Keys<'_>,
    elapsed: chrono::TimeDelta,
) -> Result<Vec<Verified<State>>, Error> {
    let now = Utc::now();
    let states = history::inspect::<State>(client, keys, now).await?;
    let mut replacements = Vec::new();
    for state in states {
        let record = creation(client, keys, state.id, now)
            .await?
            .ok_or(Error::Integrity(
                "a fixture holder retains its first record",
            ))?;
        let Some(assignment) = record
            .assignment()
            .filter(|value| value.source == Source::Registration)
        else {
            continue;
        };
        let dated = history::Seed::record::<Record>(
            state.id,
            1,
            RecordData {
                person_id: record.person_id,
                role_id: record.role_id,
                assignment_id: record.assignment_id,
                happened_at: record.happened_at - elapsed,
                operation_id: record.operation_id,
                event: Event::Appointment(Assignment {
                    id: assignment.id,
                    previous_id: assignment.previous_id,
                    change_id: assignment.change_id,
                    operator_id: assignment.operator_id,
                    revision_id: assignment.revision_id,
                    source: assignment.source,
                }),
            },
            keys,
        )?;
        client.execute(
            "UPDATE access.appointments SET assigned_at=$2,seal=$3,seal_key_id=$4,seal_version=$5 WHERE id=$1",
            &[&dated.assignment_id, &dated.happened_at, &dated.seal().tag(), &dated.seal().key_id(), &dated.seal().version()],
        ).await?;
        if state.sequence == 1 {
            let dated = history::Seed::state::<State>(
                Data {
                    id: state.id,
                    person_id: state.person_id,
                    role_id: state.role_id,
                    assignment_id: state.assignment_id,
                    cancellation_id: state.cancellation_id,
                    changed_at: state.changed_at - elapsed,
                },
                state.sequence,
                state.version,
                keys,
            )?;
            client.execute(
                "UPDATE access.holders SET changed_at=$2,seal=$3,seal_key_id=$4,seal_version=$5 WHERE id=$1",
                &[&dated.id, &dated.changed_at, &dated.seal().tag(), &dated.seal().key_id(), &dated.seal().version()],
            ).await?;
            replacements.push(dated);
        }
    }
    Ok(replacements)
}

/// Give a reader fixture equal timestamps and deliberately opposed ID/sequence ordering.
#[cfg(test)]
pub(crate) async fn order_fixture(
    client: &Client,
    keys: Keys<'_>,
    id: account::Id,
    originals: [appointment::Id; 2],
    ids: [appointment::Id; 2],
) -> Result<Verified<State>, Error> {
    let now = Utc::now();
    let state = history::inspect::<State>(client, keys, now)
        .await?
        .into_iter()
        .find(|state| state.id == id)
        .ok_or(Error::Integrity("the ordering fixture has a holder"))?;
    let first = appointment(client, keys, originals[0], now).await?;
    let second = appointment(client, keys, originals[1], now).await?;
    for (index, record) in [&first, &second].into_iter().enumerate() {
        let assignment = record.assignment().ok_or(Error::Integrity(
            "the ordering fixture contains appointments",
        ))?;
        let previous_id = if index == 0 {
            assignment.previous_id
        } else {
            Some(ids[0])
        };
        let replacement = history::Seed::record::<Record>(
            id,
            record.sequence,
            RecordData {
                person_id: record.person_id,
                role_id: record.role_id,
                assignment_id: ids[index],
                happened_at: second.happened_at,
                operation_id: record.operation_id,
                event: Event::Appointment(Assignment {
                    id: ids[index],
                    previous_id,
                    change_id: assignment.change_id,
                    operator_id: assignment.operator_id,
                    revision_id: assignment.revision_id,
                    source: assignment.source,
                }),
            },
            keys,
        )?;
        client.execute(
            "UPDATE access.appointments SET id=$2,previous_id=$3,assigned_at=$4,seal=$5,seal_key_id=$6,seal_version=$7 WHERE id=$1",
            &[&originals[index], &ids[index], &previous_id, &second.happened_at,
                &replacement.seal().tag(), &replacement.seal().key_id(), &replacement.seal().version()],
        ).await?;
    }
    let replacement = history::Seed::state::<State>(
        Data {
            id,
            person_id: state.person_id,
            role_id: state.role_id,
            assignment_id: ids[1],
            cancellation_id: None,
            changed_at: second.happened_at,
        },
        state.sequence,
        state.version,
        keys,
    )?;
    client.execute(
        "UPDATE access.holders SET assignment_id=$2,seal=$3,seal_key_id=$4,seal_version=$5 WHERE id=$1",
        &[&id, &ids[1], &replacement.seal().tag(), &replacement.seal().key_id(), &replacement.seal().version()],
    ).await?;
    Ok(replacement)
}
