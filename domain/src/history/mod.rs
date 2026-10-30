//! Authenticated state and records. Business owners declare fixed kinds and pure transitions;
//! this module alone constructs verified values and advances the process's freshness anchors.

mod anchor;
#[cfg(test)]
mod guard;

use std::{future::Future, ops::Deref, time::Duration};

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, GenericClient, Row, types::ToSql};
use zeroize::Zeroizing;

pub(crate) use crate::history::anchor::{Anchors, Pending, View};
use crate::{
    database::{Query, verification},
    kernel::keys,
    keys::seal::{self, Seal},
};

/// Possession permits decoding or persisting a kind, never constructing a verified result.
pub(crate) struct Token(());

#[derive(Clone, Copy)]
pub(crate) struct Keys<'a> {
    pub(crate) data: &'a keys::Keys,
    pub(crate) seals: &'a seal::Keys,
}

/// The declared field order is frozen by the kind's name and version.
pub(crate) trait Kind: Sized {
    type Error: From<Error>;
    const NAME: &'static str;
    const VERSION: i16 = 1;
    const SELECT: &'static str;

    fn decode(
        token: Token,
        row: &Row,
        keys: Keys<'_>,
        now: DateTime<Utc>,
    ) -> Result<Self, Self::Error>;
    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>>;
}

/// A state kind owns its business columns and fixed statements, but not its counters.
pub(crate) trait State: Kind<Error = Error> + Clone + Send + Sync + 'static {
    type Id: Copy + Eq + ToSql + Send + Sync;
    type Data;
    type Record: Record<Object = Self::Id>;

    fn build(token: Token, data: Self::Data, sequence: i64, version: i64) -> Self;
    fn id(&self) -> Self::Id;
    fn bytes(id: Self::Id) -> [u8; 16];
    fn sequence(&self) -> i64;
    fn version(&self) -> i64;
    fn has_head(&self, record: &Self::Record) -> bool;

    /// Unique business locations currently occupied by this object.
    fn locators(&self) -> Vec<Locator> {
        Vec::new()
    }

    fn write(
        &self,
        token: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> impl Future<Output = Result<u64, Error>> + Send;
}

pub(crate) trait Record: Kind<Error = Error> + Send + Sync + 'static {
    const OBJECT: &'static str;
    type Object: Copy + ToSql + Send + Sync;
    type Data;

    fn build(token: Token, object: Self::Object, sequence: i64, data: Self::Data) -> Self;
    fn object(&self) -> Self::Object;
    fn sequence(&self) -> i64;
    fn operation_id(&self) -> crate::identity::operation::Id;
    /// Kinds with an explicit predecessor bind it to the preceding authenticated record.
    fn follows(&self, _: &Self) -> bool {
        true
    }
    fn insert(
        &self,
        token: Token,
        client: &Client,
        keys: Keys<'_>,
        seal: &Seal,
    ) -> impl Future<Output = Result<(), Error>> + Send;
}

/// Immutable values are decoded and written through the same authenticated boundary as records.
pub(crate) trait Value: Kind + Send + Sync + 'static {
    type Id: Copy + ToSql + Send + Sync;
    type Data;

    fn build(token: Token, data: Self::Data) -> Self;
    fn id(&self) -> Self::Id;
    fn insert(
        &self,
        token: Token,
        client: &(impl GenericClient + Sync),
        seal: &Seal,
    ) -> impl Future<Output = Result<(), Error>> + Send;
}

/// Only short-lived state kinds may delete their complete record group.
pub(crate) trait Removable: State {
    fn remove(
        token: Token,
        client: &Client,
        original: &Verified<Self>,
    ) -> impl Future<Output = Result<u64, Error>> + Send;
}

/// Only authenticated loading or the test Seed can produce this read-only qualification.
#[derive(Clone)]
pub(crate) struct Verified<T> {
    value: T,
    seal: Seal,
}

impl<T> Deref for Verified<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> Verified<T> {
    pub(crate) fn seal(&self) -> &Seal {
        &self.seal
    }

    pub(crate) fn into_inner(self) -> T {
        self.value
    }
}

/// Create or advance a state with one paired record.
pub(crate) struct Draft<S: State> {
    original: Option<Verified<S>>,
    value: S,
}

/// Append a newly registered candidate without replacing an earlier one.
pub(crate) fn retain(pending: &mut Option<Pending>, next: Pending) -> Result<(), Error> {
    match pending {
        Some(pending) => pending.extend(next),
        slot @ None => {
            *slot = Some(next);
            Ok(())
        }
    }
}

impl<S: State> Draft<S> {
    pub(crate) fn create(data: S::Data) -> Self {
        Self {
            original: None,
            value: S::build(Token(()), data, 1, 1),
        }
    }

    pub(crate) fn advance(original: &Verified<S>, data: S::Data) -> Result<Self, Error> {
        let sequence = original
            .sequence()
            .checked_add(1)
            .ok_or(Error::Integrity("state sequence is representable"))?;
        let version = original
            .version()
            .checked_add(1)
            .ok_or(Error::Integrity("state version is representable"))?;
        let value = S::build(Token(()), data, sequence, version);
        if original.id() != value.id() {
            return Err(Error::Integrity("a state transition preserves its object"));
        }
        Ok(Self {
            original: Some(original.clone()),
            value,
        })
    }
}

impl<S: Removable> Draft<S> {
    pub(crate) fn remove(
        original: &Verified<S>,
        operation: crate::identity::operation::Id,
    ) -> Removal<S> {
        Removal {
            original: original.clone(),
            operation,
        }
    }
}

pub(crate) struct Removal<S: Removable> {
    original: Verified<S>,
    operation: crate::identity::operation::Id,
}

impl<S: Removable> Removal<S> {
    pub(crate) async fn write(self, client: &Client, anchors: &Anchors) -> Result<Pending, Error> {
        let object = Object::of::<S>(self.original.id());
        let stamp = Stamp::of(&self.original);
        anchors.check(object, Some(stamp.clone()), Read::Locked)?;
        if S::remove(Token(()), client, &self.original).await? != 1 {
            return Err(Error::Integrity(
                "state deletion matches its verified version and seal",
            ));
        }
        anchors.remove(object, self.operation, stamp)
    }
}

/// State and its immutable records are written together before COMMIT is sent.
pub(crate) struct Change<S: State> {
    draft: Draft<S>,
    preceding: Vec<S::Record>,
    record: S::Record,
}

impl<S: State> Change<S> {
    pub(crate) fn new(draft: Draft<S>, data: <S::Record as Record>::Data) -> Self {
        let record = S::Record::build(Token(()), draft.value.id(), draft.value.sequence(), data);
        Self {
            draft,
            preceding: Vec::new(),
            record,
        }
    }

    /// One transaction may append several records while writing the state only once.
    pub(crate) fn multiple(
        original: Option<&Verified<S>>,
        data: S::Data,
        preceding: Vec<<S::Record as Record>::Data>,
        last: <S::Record as Record>::Data,
    ) -> Result<Self, Error> {
        let count = i64::try_from(preceding.len())
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or(Error::Integrity("record count is representable"))?;
        let first = original.map_or(0, |state| state.sequence());
        let sequence = first
            .checked_add(count)
            .ok_or(Error::Integrity("state sequence is representable"))?;
        let version = original
            .map_or(0, |state| state.version())
            .checked_add(1)
            .ok_or(Error::Integrity("state version is representable"))?;
        let value = S::build(Token(()), data, sequence, version);
        if original.is_some_and(|state| state.id() != value.id()) {
            return Err(Error::Integrity("a state transition preserves its object"));
        }
        let mut records = Vec::with_capacity(preceding.len());
        let mut sequence = first;
        for data in preceding {
            sequence += 1;
            records.push(S::Record::build(Token(()), value.id(), sequence, data));
        }
        let record = S::Record::build(Token(()), value.id(), value.sequence(), last);
        if records
            .iter()
            .any(|item| item.operation_id() != record.operation_id())
        {
            return Err(Error::Integrity(
                "one state change belongs to one original operation",
            ));
        }
        Ok(Self {
            draft: Draft {
                original: original.cloned(),
                value,
            },
            preceding: records,
            record,
        })
    }

    pub(crate) async fn write(
        self,
        client: &Client,
        keys: Keys<'_>,
        anchors: &Anchors,
    ) -> Result<Pending, Error> {
        let state_seal = self.persist(client, keys).await?;
        anchors.prepare(
            Object::of::<S>(self.draft.value.id()),
            self.record.operation_id(),
            self.record.sequence(),
            self.draft.original.as_ref().map(Stamp::of),
            Stamp {
                version: self.draft.value.version(),
                seal: state_seal,
                locators: self.draft.value.locators(),
            },
        )
    }

    /// A fixture owns the outer transaction; startup later authenticates these rows.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn write_offline(self, client: &Client, keys: Keys<'_>) -> Result<(), Error> {
        self.persist(client, keys).await?;
        Ok(())
    }

    async fn persist(&self, client: &Client, keys: Keys<'_>) -> Result<Seal, Error> {
        let state_seal = sign(keys.seals, &self.draft.value)?;
        for record in &self.preceding {
            let seal = sign(keys.seals, record)?;
            record.insert(Token(()), client, keys, &seal).await?;
        }
        let record_seal = sign(keys.seals, &self.record)?;
        self.record
            .insert(Token(()), client, keys, &record_seal)
            .await?;
        if self
            .draft
            .value
            .write(Token(()), client, &state_seal, self.draft.original.as_ref())
            .await?
            != 1
        {
            return Err(Error::Integrity(
                "state update matches its verified version and seal",
            ));
        }
        Ok(state_seal)
    }
}

fn sign<K: Kind>(keys: &seal::Keys, value: &K) -> Result<Seal, Error> {
    let fields = value.fields();
    Ok(keys.sign(
        K::NAME,
        K::VERSION,
        &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
    )?)
}

fn decode<K: Kind>(row: &Row, keys: Keys<'_>, now: DateTime<Utc>) -> Result<Verified<K>, K::Error> {
    let seal = Seal::stored(
        row.try_get("seal").map_err(Error::decode)?,
        row.try_get("seal_key_id").map_err(Error::decode)?,
        row.try_get("seal_version").map_err(Error::decode)?,
    )
    .map_err(Error::from)?;
    let value = K::decode(Token(()), row, keys, now)?;
    let fields = value.fields();
    keys.seals
        .check(
            K::NAME,
            K::VERSION,
            &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            &seal,
        )
        .map_err(Error::from)?;
    Ok(Verified { value, seal })
}

/// Authenticate a record selected by its owner's fixed identity or history query.
pub(crate) fn record<R: Record>(
    row: &Row,
    keys: Keys<'_>,
    now: DateTime<Utc>,
) -> Result<Verified<R>, Error> {
    decode(row, keys, now)
}

pub(crate) async fn insert_value<V: Value>(
    client: &(impl GenericClient + Sync),
    keys: Keys<'_>,
    data: V::Data,
) -> Result<(), Error> {
    let value = V::build(Token(()), data);
    let seal = sign(keys.seals, &value)?;
    value.insert(Token(()), client, &seal).await
}

pub(crate) async fn find_value<V: Value>(
    client: &impl GenericClient,
    keys: Keys<'_>,
    id: V::Id,
    now: DateTime<Utc>,
) -> Result<Option<Verified<V>>, V::Error> {
    client
        .query_opt(&format!("{} WHERE value.id = $1", V::SELECT), &[&id])
        .await
        .map_err(Error::from)?
        .as_ref()
        .map(|row| decode::<V>(row, keys, now))
        .transpose()
}

/// Authenticate a caller's fixed references in one query; the owner decides required absence.
pub(crate) async fn find_values<V: Value>(
    client: &impl GenericClient,
    keys: Keys<'_>,
    ids: &[V::Id],
    now: DateTime<Utc>,
) -> Result<Vec<Verified<V>>, V::Error> {
    let rows = client
        .query(&format!("{} WHERE value.id = ANY($1)", V::SELECT), &[&ids])
        .await
        .map_err(Error::from)?;
    rows.iter().map(|row| decode::<V>(row, keys, now)).collect()
}

pub(crate) async fn inspect_values<V: Value>(
    client: &impl GenericClient,
    keys: Keys<'_>,
    now: DateTime<Utc>,
) -> Result<(), V::Error> {
    let mut cursor: Option<V::Id> = None;
    loop {
        let rows = verification::Stage::HistoryRecords
            .batch(
                Duration::from_secs(10),
                client.query(
                    &format!(
                        "{} WHERE $1::uuid IS NULL OR value.id > $1 ORDER BY value.id LIMIT 128",
                        V::SELECT
                    ),
                    &[&cursor],
                ),
            )
            .await
            .map_err(Error::from)?
            .map_err(Error::from)?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in rows {
            cursor = Some(decode::<V>(&row, keys, now)?.id());
        }
    }
}

/// Joined identity reads defer this step until their original session is confirmed.
pub(crate) fn state<S: State>(
    row: &Row,
    keys: Keys<'_>,
    anchors: &Anchors,
    read: Read,
    now: DateTime<Utc>,
) -> Result<Verified<S>, Error> {
    let value = decode::<S>(row, keys, now)?;
    if value.sequence() < 1 || value.version() < 1 {
        return Err(Error::Integrity("state counters are positive"));
    }
    anchors.check(Object::of::<S>(value.id()), Some(Stamp::of(&value)), read)?;
    Ok(value)
}

/// Authenticate a fixed owner query, including a missing row at an occupied location.
pub(crate) fn located<S: State>(
    row: Option<&Row>,
    locator: &Locator,
    keys: Keys<'_>,
    anchors: &Anchors,
    read: Read,
    now: DateTime<Utc>,
) -> Result<Option<Verified<S>>, Error> {
    if locator.kind != S::NAME {
        return Err(Error::Integrity("a locator names its state kind"));
    }
    let value = row
        .map(|row| state::<S>(row, keys, anchors, read, now))
        .transpose()?;
    if let Some(value) = &value
        && !value.locators().contains(locator)
    {
        return Err(Error::Integrity(
            "a located state occupies the requested location",
        ));
    }
    anchors.check_locator(
        locator,
        value.as_ref().map(|value| Object::of::<S>(value.id())),
        read,
    )?;
    Ok(value)
}

/// Compound unique locations let a business owner authenticate a complete related set.
pub(crate) fn related<S: State>(
    rows: &[Row],
    prefix: &Locator,
    keys: Keys<'_>,
    anchors: &Anchors,
    read: Read,
    now: DateTime<Utc>,
) -> Result<Vec<Verified<S>>, Error> {
    if prefix.kind != S::NAME {
        return Err(Error::Integrity("a related location names its state kind"));
    }
    let mut values = Vec::with_capacity(rows.len());
    let mut observed = std::collections::HashMap::with_capacity(rows.len());
    for row in rows {
        let value = state::<S>(row, keys, anchors, read, now)?;
        let mut matched = false;
        for locator in value.locators() {
            if locator.within(prefix) {
                matched = true;
                if observed
                    .insert(locator, Object::of::<S>(value.id()))
                    .is_some()
                {
                    return Err(Error::Integrity(
                        "a related state set has unique compound locations",
                    ));
                }
            }
        }
        if !matched {
            return Err(Error::Integrity(
                "every related state belongs to the requested location",
            ));
        }
        values.push(value);
    }
    anchors.check_related(prefix, &observed, read)?;
    Ok(values)
}

/// A complete state collection also checks anchored objects omitted from SQL results.
pub(crate) async fn all<S: State>(
    client: &Client,
    keys: Keys<'_>,
    anchors: &Anchors,
    read: Read,
    now: DateTime<Utc>,
) -> Result<Vec<Verified<S>>, Error> {
    let lock = match read {
        Read::Locked => " FOR UPDATE OF state",
        Read::Shared => " FOR SHARE OF state",
        Read::Snapshot(_) => "",
    };
    let rows = client.query(&format!("{}{lock}", S::SELECT), &[]).await?;
    let mut values = Vec::with_capacity(rows.len());
    let mut seen = std::collections::HashSet::with_capacity(rows.len());
    for row in &rows {
        let value = state::<S>(row, keys, anchors, read, now)?;
        if !seen.insert(S::bytes(value.id())) {
            return Err(Error::Integrity(
                "a state collection has unique object identities",
            ));
        }
        values.push(value);
    }
    for object in anchors.objects::<S>()? {
        if !seen.contains(&object.id) {
            anchors.check(object, None, read)?;
        }
    }
    Ok(values)
}

/// Object locks precede locked reads; views are taken before their SQL snapshot.
#[derive(Clone, Copy)]
pub(crate) enum Read {
    Locked,
    Shared,
    Snapshot(View),
}

pub(crate) async fn find<S: State>(
    client: &Client,
    keys: Keys<'_>,
    anchors: &Anchors,
    read: Read,
    id: S::Id,
    now: DateTime<Utc>,
) -> Result<Option<Verified<S>>, Error> {
    let lock = match read {
        Read::Locked => " FOR UPDATE OF state",
        Read::Shared => " FOR SHARE OF state",
        Read::Snapshot(_) => "",
    };
    let option = client
        .query_opt(&format!("{} WHERE state.id = $1{lock}", S::SELECT), &[&id])
        .await?;
    let option = option
        .as_ref()
        .map(|row| decode::<S>(row, keys, now))
        .transpose()?;
    if let Some(state) = &option
        && (state.id() != id || state.sequence() < 1 || state.version() < 1)
    {
        return Err(Error::Integrity(
            "state has its selected identity and positive counters",
        ));
    }
    anchors.check(Object::of::<S>(id), option.as_ref().map(Stamp::of), read)?;
    Ok(option)
}

/// Original-operation reads authenticate the record without reading today's state.
pub(crate) async fn original<R: Record>(
    client: &Client,
    keys: Keys<'_>,
    operation: crate::identity::operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<Verified<R>>, Error> {
    let option = client
        .query_opt(
            &format!("{} WHERE record.operation_id = $1", R::SELECT),
            &[&operation],
        )
        .await?;
    option
        .as_ref()
        .map(|row| decode::<R>(row, keys, now))
        .transpose()
}

/// A complete requested window has exactly the sequence numbers implied by its state.
pub(crate) async fn page<S: State>(
    client: &Client,
    keys: Keys<'_>,
    state: &Verified<S>,
    size: i64,
    offset: i64,
    now: DateTime<Utc>,
) -> Result<Vec<Verified<S::Record>>, Error> {
    let rows = client
        .query(
            &format!(
                "{} WHERE record.{} = $1 ORDER BY record.sequence DESC LIMIT $2 OFFSET $3",
                S::Record::SELECT,
                S::Record::OBJECT
            ),
            &[&state.id(), &size, &offset],
        )
        .await?;
    let end = state.sequence().saturating_sub(offset).max(0);
    let count = end.min(size);
    if i64::try_from(rows.len()).ok() != Some(count) {
        return Err(Error::Integrity(
            "history window contains every expected sequence",
        ));
    }
    let mut records = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let record = decode::<S::Record>(row, keys, now)?;
        let index =
            i64::try_from(index).map_err(|_| Error::Integrity("history index is representable"))?;
        if S::bytes(record.object()) != S::bytes(state.id())
            || record.sequence() != end - index
            || (offset == 0 && index == 0 && !state.has_head(&record))
        {
            return Err(Error::Integrity(
                "history window agrees with its object's current sequence",
            ));
        }
        records.push(record);
    }
    Ok(records)
}

/// Startup authenticates the entire kind and its contiguous records before installing anchors.
pub(crate) async fn start<S: State>(
    client: &Client,
    keys: Keys<'_>,
    anchors: &Anchors,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    initialize(anchors, inspect::<S>(client, keys, now).await?)
}

/// Installs only authenticated startup facts after the owning capability's audit.
pub(crate) fn initialize<S: State>(
    anchors: &Anchors,
    states: impl IntoIterator<Item = Verified<S>>,
) -> Result<(), Error> {
    for state in states {
        anchors.initialize(Object::of::<S>(state.id()), Stamp::of(&state))?;
    }
    Ok(())
}

/// Full authentication under the caller's startup or offline maintenance boundary.
pub(crate) async fn inspect<S: State>(
    client: &Client,
    keys: Keys<'_>,
    now: DateTime<Utc>,
) -> Result<Vec<Verified<S>>, Error> {
    let mut states = std::collections::BTreeMap::new();
    let mut cursor: Option<S::Id> = None;
    loop {
        let sql = format!(
            "{} WHERE $1::uuid IS NULL OR state.id > $1 ORDER BY state.id LIMIT 128",
            S::SELECT
        );
        let rows = verification::Stage::HistoryStates
            .batch(Duration::from_secs(10), client.query(&sql, &[&cursor]))
            .await??;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let state = decode::<S>(&row, keys, now)?;
            cursor = Some(state.id());
            if state.sequence() < 1
                || state.version() < 1
                || states
                    .insert(S::bytes(state.id()), (state, 0_i64))
                    .is_some()
            {
                return Err(Error::Integrity(
                    "state identities are unique and counters are positive",
                ));
            }
        }
    }
    let mut cursor: Option<S::Id> = None;
    let mut position = 0_i64;
    let mut previous: Option<Verified<S::Record>> = None;
    loop {
        let sql = format!(
            "{} WHERE $1::uuid IS NULL OR (record.{}, record.sequence) > ($1, $2) ORDER BY record.{}, record.sequence LIMIT 128",
            S::Record::SELECT,
            S::Record::OBJECT,
            S::Record::OBJECT
        );
        let rows = verification::Stage::HistoryRecords
            .batch(
                Duration::from_secs(10),
                client.query(&sql, &[&cursor, &position]),
            )
            .await??;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let record = decode::<S::Record>(&row, keys, now)?;
            cursor = Some(record.object());
            position = record.sequence();
            let (state, sequence) = states
                .get_mut(&S::bytes(record.object()))
                .ok_or(Error::Integrity("every record belongs to a retained state"))?;
            *sequence = sequence
                .checked_add(1)
                .ok_or(Error::Integrity("record sequence is representable"))?;
            if record.sequence() != *sequence
                || *sequence > state.sequence()
                || (*sequence == state.sequence() && !state.has_head(&record))
                || (*sequence > 1
                    && !previous.as_ref().is_some_and(|previous| {
                        previous.object() == record.object() && record.follows(previous)
                    }))
            {
                return Err(Error::Integrity(
                    "records run contiguously from one to the state's head",
                ));
            }
            previous = Some(record);
        }
    }
    let mut values = Vec::with_capacity(states.len());
    for (_, (state, sequence)) in states {
        if sequence != state.sequence() {
            return Err(Error::Integrity("state sequence equals the final record"));
        }
        values.push(state);
    }
    Ok(values)
}

/// Test facts bypass business scenarios but retain the same counters, seals and paired writes.
#[cfg(any(test, feature = "test-support"))]
pub(crate) struct Seed;

#[cfg(any(test, feature = "test-support"))]
impl Seed {
    #[cfg(test)]
    pub(crate) fn value<V: Value>(data: V::Data, keys: Keys<'_>) -> Result<Verified<V>, Error> {
        let value = V::build(Token(()), data);
        let seal = sign(keys.seals, &value)?;
        Ok(Verified { value, seal })
    }

    #[cfg(test)]
    pub(crate) async fn persist_state<S: State>(
        client: &Client,
        state: &Verified<S>,
        original: Option<&Verified<S>>,
    ) -> Result<(), Error> {
        if state
            .write(Token(()), client, state.seal(), original)
            .await?
            != 1
        {
            return Err(Error::Integrity("a fixture writes its selected state"));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn persist_record<R: Record>(
        client: &Client,
        keys: Keys<'_>,
        record: &Verified<R>,
    ) -> Result<(), Error> {
        record.insert(Token(()), client, keys, record.seal()).await
    }

    pub(crate) fn state<S: State>(
        data: S::Data,
        sequence: i64,
        version: i64,
        keys: Keys<'_>,
    ) -> Result<Verified<S>, Error> {
        let value = S::build(Token(()), data, sequence, version);
        let seal = sign(keys.seals, &value)?;
        Ok(Verified { value, seal })
    }

    pub(crate) fn record<R: Record>(
        object: R::Object,
        sequence: i64,
        data: R::Data,
        keys: Keys<'_>,
    ) -> Result<Verified<R>, Error> {
        let value = R::build(Token(()), object, sequence, data);
        let seal = sign(keys.seals, &value)?;
        Ok(Verified { value, seal })
    }

    /// Inspect a pure transition using the production kinds without opening a database.
    #[cfg(test)]
    pub(crate) fn change<S: State>(
        change: Change<S>,
        keys: Keys<'_>,
    ) -> Result<(Verified<S>, Verified<S::Record>), Error> {
        if !change.preceding.is_empty() {
            return Err(Error::Integrity(
                "a single-record fixture retains every record",
            ));
        }
        let state = change.draft.value;
        let record = change.record;
        let state_seal = sign(keys.seals, &state)?;
        let record_seal = sign(keys.seals, &record)?;
        Ok((
            Verified {
                value: state,
                seal: state_seal,
            },
            Verified {
                value: record,
                seal: record_seal,
            },
        ))
    }

    /// Populate disposable databases through the same kind encodings and fixed writes.
    #[cfg(test)]
    pub(crate) async fn write<S: State>(
        client: &Client,
        keys: Keys<'_>,
        change: Change<S>,
    ) -> Result<Verified<S>, Error> {
        let seal = change.persist(client, keys).await?;
        let state = change.draft.value;
        Ok(Verified { value: state, seal })
    }

    /// Call only after committing a fixture, with no concurrent application work.
    pub(crate) fn anchor<S: State>(anchors: &Anchors, state: &Verified<S>) -> Result<(), Error> {
        anchors.seed(Object::of::<S>(state.id()), Stamp::of(state))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Object {
    kind: &'static str,
    id: [u8; 16],
}

impl Object {
    fn of<S: State>(id: S::Id) -> Self {
        Self {
            kind: S::NAME,
            id: S::bytes(id),
        }
    }
}

/// A kind owns the name and exact bytes of each of its unique lookup keys.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Locator {
    kind: &'static str,
    name: &'static str,
    value: Vec<u8>,
}

impl Locator {
    pub(crate) fn new<S: State>(name: &'static str, value: &[u8]) -> Self {
        Self {
            kind: S::NAME,
            name,
            value: value.to_vec(),
        }
    }

    fn within(&self, prefix: &Self) -> bool {
        self.kind == prefix.kind
            && self.name == prefix.name
            && self.value.starts_with(&prefix.value)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    version: i64,
    seal: Seal,
    locators: Vec<Locator>,
}

impl Stamp {
    fn of<S: State>(state: &Verified<S>) -> Self {
        Self {
            version: state.version(),
            seal: state.seal.clone(),
            locators: state.locators(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("history startup verification exceeded its batch budget")]
    Timeout(#[from] verification::Timeout),
    #[error("history query failed: {0}")]
    Query(#[from] Query),
    #[error("history decoding failed: {0}")]
    Decode(Query),
    #[error("{0}")]
    Seal(#[from] seal::Error),
    #[error("{0}")]
    Encryption(#[from] keys::Error),
    #[error("history plaintext is not UTF-8")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("history value is outside its vocabulary")]
    Value(#[from] strum::ParseError),
    #[error("history integrity failed: {0}")]
    Integrity(&'static str),
    #[error("the anchor changed after the read view; retry the complete snapshot once")]
    Changed,
    #[error("the anchor changed after the read view and ending that snapshot failed: {0}")]
    Retry(#[source] Query),
}

impl Error {
    pub(crate) fn corrupt(&self) -> bool {
        !matches!(
            self,
            Self::Query(_) | Self::Changed | Self::Timeout(_) | Self::Retry(_)
        )
    }

    pub(crate) fn decode(error: tokio_postgres::Error) -> Self {
        Self::Decode(Query(error))
    }
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(Query(error))
    }
}

impl<R> From<Error> for crate::kernel::Error<R> {
    fn from(error: Error) -> Self {
        use crate::kernel::{Error as Failure, Fault, FaultKind, Stage};
        let corrupt = error.corrupt();
        let fault = Fault::new(Stage::Command, FaultKind::History(error));
        if corrupt {
            Failure::Corrupt(fault)
        } else {
            Failure::Unavailable(fault)
        }
    }
}
