//! Account eligibility and unique occupancy, authenticated independently of its history.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row, types::ToSql};
use zeroize::Zeroizing;

use crate::{
    history::{self, Anchors, Error, Keys, Kind, Locator, Read, Token, Verified},
    identity::{
        account::{Id, record::Record},
        avatar, cancellation,
        mobile::{
            self, Mobile,
            envelope::{Binding, Envelope},
        },
        person::{self, number::hash::Hash},
        suspension, wechat,
    },
    keys::seal::Seal,
};

pub(crate) const TABLE: &str = "identity.accounts";

pub(crate) async fn lock(client: &Client, id: Id) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE",
            &[&id],
        )
        .await?;
    Ok(())
}

pub(crate) async fn lock_shared(client: &Client, id: Id) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM identity.accounts WHERE id=$1 FOR SHARE",
            &[&id],
        )
        .await?;
    Ok(())
}

/// The person object lock follows the account lock and precedes session locks.
pub(crate) async fn lock_person(client: &Client, id: Id) -> Result<(), Error> {
    if let Some(person) = discover_owner(client, id).await.map_err(Error::from)? {
        crate::identity::person::state::lock(client, person).await?;
    }
    Ok(())
}

/// The actor is stable while the target may change, in the global account order.
pub(crate) async fn lock_management(client: &Client, actor: Id, target: Id) -> Result<(), Error> {
    if actor == target {
        lock(client, target).await?;
    } else if actor.as_bytes() < target.as_bytes() {
        lock_shared(client, actor).await?;
        lock(client, target).await?;
    } else {
        lock(client, target).await?;
        lock_shared(client, actor).await?;
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct State {
    data: Data,
    sequence: i64,
    version: i64,
}

#[derive(Clone)]
pub(crate) struct Data {
    pub(super) id: Id,
    pub(super) person_id: person::Id,
    pub(super) status: Status,
    pub(super) avatar: avatar::Revision,
    pub(super) wechat: wechat::Revision,
    pub(super) created_at: DateTime<Utc>,
    pub(super) changed_at: DateTime<Utc>,
}

#[derive(Clone)]
pub(super) enum Status {
    Current(Current),
    Cancelled(cancellation::Id),
}

#[derive(Clone)]
pub(crate) struct Current {
    pub(super) mobile: Arc<Envelope>,
    pub(super) plaintext: Arc<Mobile>,
    pub(super) number: Arc<Hash>,
    pub(super) suspension_id: Option<suspension::Id>,
}

pub(super) fn open_mobile(
    mobile: &Envelope,
    keys: &crate::kernel::keys::Keys,
    id: Id,
) -> Result<Arc<Mobile>, Error> {
    let plaintext = keys.open(mobile.ciphertext(), &Binding::Account(id))?;
    let mobile = std::str::from_utf8(&plaintext)?
        .parse()
        .map_err(|_: mobile::ParseError| {
            Error::Integrity("account mobile plaintext is canonical")
        })?;
    Ok(Arc::new(mobile))
}

impl Current {
    pub(crate) fn mobile(&self) -> &Envelope {
        &self.mobile
    }

    pub(crate) fn number(&self) -> &Hash {
        &self.number
    }

    pub(crate) fn suspension_id(&self) -> Option<suspension::Id> {
        self.suspension_id
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(super) async fn write_aged(client: &Client, state: &Verified<State>) -> Result<(), Error> {
    client.execute(
        "UPDATE identity.accounts SET created_at=$2,changed_at=$3,seal=$4,seal_key_id=$5,seal_version=$6 WHERE id=$1",
        &[&state.id(), &state.created_at(), &state.changed_at(), &state.seal().tag(),
          &state.seal().key_id(), &state.seal().version()],
    ).await?;
    Ok(())
}

/// Rekey one stopped, disposable account inside the fixture's offline transaction.
#[cfg(any(test, feature = "test-support"))]
pub async fn rekey_for_test(
    client: &Client,
    old: &crate::keys::Keys,
    new: &crate::keys::Keys,
    id: Id,
    number: &person::number::IdNumber,
) -> Result<(), super::ReadError> {
    let now = client
        .query_one("SELECT clock_timestamp()", &[])
        .await?
        .try_get(0)?;
    let state = history::inspect::<State>(
        client,
        Keys {
            data: &old.encryption,
            seals: &old.seal,
        },
        now,
    )
    .await?
    .into_iter()
    .find(|state| state.id() == id)
    .ok_or(super::ReadError::Facts)?;
    let mut data = state.data();
    let Status::Current(current) = &mut data.status else {
        return Err(super::ReadError::Facts);
    };
    if current.number().as_ref() != old.identity.number.hash(number)?.as_ref() {
        return Err(super::ReadError::Facts);
    }
    let mobile = current.mobile.open(
        &old.identity.encryption,
        &old.identity.mobile,
        Binding::Account(id),
    )?;
    current.mobile = Arc::new(Envelope::seal(
        &new.identity.encryption,
        &new.identity.mobile,
        Binding::Account(id),
        &mobile,
    )?);
    current.number = Arc::new(new.identity.number.hash(number)?);
    let resealed = history::Seed::state::<State>(
        data,
        state.sequence,
        state.version,
        Keys {
            data: &new.encryption,
            seals: &new.seal,
        },
    )?;
    let current = resealed.current().ok_or(super::ReadError::Facts)?;
    let updated = client.execute(
        "UPDATE identity.accounts SET mobile=$2,id_number=$3,seal=$4,seal_key_id=$5,seal_version=$6 WHERE id=$1",
        &[&id, &current.mobile(), &current.number(), &resealed.seal().tag(),
          &resealed.seal().key_id(), &resealed.seal().version()],
    ).await?;
    if updated != 1 {
        return Err(super::ReadError::Facts);
    }
    Ok(())
}

impl State {
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn aged(
        &self,
        keys: Keys<'_>,
        elapsed: chrono::TimeDelta,
        registration: bool,
    ) -> Result<Verified<Self>, Error> {
        let mut data = self.data.clone();
        if registration {
            data.created_at -= elapsed;
        }
        data.changed_at -= elapsed;
        if data.changed_at < data.created_at {
            return Err(Error::Integrity(
                "historical SMS changes remain after the fixed bootstrap origin",
            ));
        }
        history::Seed::state::<Self>(data, self.sequence, self.version, keys)
    }

    pub(crate) fn id(&self) -> Id {
        self.data.id
    }

    pub(crate) fn person_id(&self) -> person::Id {
        self.data.person_id
    }

    pub(crate) fn current(&self) -> Option<&Current> {
        match &self.data.status {
            Status::Current(current) => Some(current),
            Status::Cancelled(_) => None,
        }
    }

    pub(crate) fn cancellation_id(&self) -> Option<cancellation::Id> {
        match self.data.status {
            Status::Current(_) => None,
            Status::Cancelled(id) => Some(id),
        }
    }

    pub(crate) fn avatar(&self) -> avatar::Revision {
        self.data.avatar
    }

    #[cfg(test)]
    pub(crate) fn wechat(&self) -> wechat::Revision {
        self.data.wechat
    }

    pub(crate) fn created_at(&self) -> DateTime<Utc> {
        self.data.created_at
    }

    pub(crate) fn changed_at(&self) -> DateTime<Utc> {
        self.data.changed_at
    }

    pub(super) fn data(&self) -> Data {
        self.data.clone()
    }
}

pub(crate) enum Location<'a> {
    Person(person::Id),
    Mobile(&'a mobile::hash::Hash),
    Number(&'a Hash),
}

/// Discovers only a lock identity. The caller must authenticate the location after locking.
pub(crate) async fn discover_person(
    client: &Client,
    id: person::Id,
) -> Result<Option<Id>, crate::database::Query> {
    Ok(client
        .query_opt(
            "SELECT id FROM identity.accounts WHERE person_id=$1",
            &[&id],
        )
        .await?
        .map(|row| row.try_get(0))
        .transpose()?)
}

/// Selects lock participants only; a later authenticated read owns identity and qualification.
pub(crate) async fn discover_owner(
    client: &Client,
    id: Id,
) -> Result<Option<person::Id>, crate::database::Query> {
    Ok(client
        .query_opt(
            "SELECT person_id FROM identity.accounts WHERE id=$1",
            &[&id],
        )
        .await?
        .map(|row| row.try_get(0))
        .transpose()?)
}

/// Workforce recovery stabilizes the actor and both permanent personal identities together.
pub(crate) async fn lock_participants(
    client: &Client,
    actor: Id,
    persons: &[person::Id],
) -> Result<(), Error> {
    client.query(
        "SELECT id FROM identity.accounts WHERE id=$1 OR person_id=ANY($2) ORDER BY id FOR SHARE",
        &[&actor, &persons],
    ).await?;
    Ok(())
}

/// Discovers only a lock identity. The caller must authenticate the location after locking.
pub(crate) async fn discover_mobile(
    client: &Client,
    hash: &mobile::hash::Hash,
) -> Result<Option<Id>, Error> {
    Ok(client
        .query_opt(
            "SELECT id FROM identity.accounts WHERE (mobile).hash = $1",
            &[hash],
        )
        .await?
        .map(|row| row.try_get(0))
        .transpose()?)
}

/// Locks every discovered account before a replacement takes either mobile occupancy.
pub(crate) async fn lock_replacement(
    client: &Client,
    actor: Id,
    target: Id,
    occupied: Option<Id>,
) -> Result<(), Error> {
    let mut ids = vec![actor, target];
    ids.extend(occupied);
    ids.sort_by_key(|id| *id.as_bytes());
    ids.dedup();
    for id in ids {
        if id == target {
            lock(client, id).await?;
        } else {
            lock_shared(client, id).await?;
        }
    }
    Ok(())
}

/// A lock hint only. Decisive use still loads and authenticates the whole Account.
pub(crate) async fn discover_envelope(client: &Client, id: Id) -> Result<Option<Envelope>, Error> {
    client
        .query_opt("SELECT mobile FROM identity.accounts WHERE id=$1", &[&id])
        .await?
        .map(|row| row.try_get(0).map_err(Error::decode))
        .transpose()
        .map(Option::flatten)
}

/// Finds the ordered account lock set; occupancy is authenticated after those locks.
pub(crate) async fn discover_occupancies(
    client: &Client,
    mobile: &mobile::hash::Hash,
    number: &Hash,
) -> Result<Vec<Id>, Error> {
    client
        .query(
            "SELECT id FROM identity.accounts WHERE (mobile).hash=$1 OR id_number=$2 ORDER BY id",
            &[mobile, number],
        )
        .await?
        .into_iter()
        .map(discovered)
        .collect()
}

fn discovered(row: Row) -> Result<Id, Error> {
    row.try_get(0).map_err(Error::decode)
}

pub(crate) async fn located(
    client: &Client,
    keys: Keys<'_>,
    anchors: &Anchors,
    read: Read,
    location: Location<'_>,
    now: DateTime<Utc>,
) -> Result<Option<Verified<State>>, Error> {
    let (condition, value, locator): (&str, &(dyn ToSql + Sync), _) = match &location {
        Location::Person(id) => (
            "state.person_id = $1",
            id,
            Locator::new::<State>("person", id.as_bytes()),
        ),
        Location::Mobile(hash) => (
            "(state.mobile).hash = $1",
            *hash,
            Locator::new::<State>("mobile", hash.as_ref()),
        ),
        Location::Number(hash) => (
            "state.id_number = $1",
            *hash,
            Locator::new::<State>("number", hash.as_ref()),
        ),
    };
    let lock = match read {
        Read::Locked => " FOR UPDATE OF state",
        Read::Shared => " FOR SHARE OF state",
        Read::Snapshot(_) => "",
    };
    let row = client
        .query_opt(
            &format!("{} WHERE {condition}{lock}", State::SELECT),
            &[value],
        )
        .await?;
    history::located(row.as_ref(), &locator, keys, anchors, read, now)
}

pub(crate) async fn for_persons(
    client: &Client,
    keys: Keys<'_>,
    anchors: &Anchors,
    read: Read,
    persons: &[person::Id],
    now: DateTime<Utc>,
) -> Result<Vec<Option<Verified<State>>>, Error> {
    if persons.is_empty() {
        return Ok(Vec::new());
    }
    let lock = match read {
        Read::Locked => " FOR UPDATE OF state",
        Read::Shared => " FOR SHARE OF state",
        Read::Snapshot(_) => "",
    };
    let rows = client
        .query(
            &format!(
                "{} WHERE state.person_id=ANY($1) ORDER BY state.id{lock}",
                State::SELECT
            ),
            &[&persons],
        )
        .await?;
    let mut selected = HashMap::with_capacity(rows.len());
    for row in rows {
        let person_id: person::Id = row.try_get("person_id").map_err(Error::decode)?;
        if selected.insert(*person_id.as_bytes(), row).is_some() {
            return Err(Error::Integrity("each person locates at most one account"));
        }
    }
    persons
        .iter()
        .map(|person_id| {
            history::located(
                selected.get(person_id.as_bytes()),
                &Locator::new::<State>("person", person_id.as_bytes()),
                keys,
                anchors,
                read,
                now,
            )
        })
        .collect()
}

impl history::Kind for State {
    type Error = history::Error;
    const NAME: &'static str = "identity.account";
    const SELECT: &'static str = "SELECT state.id, state.person_id, state.mobile, state.id_number, state.avatar, state.wechat, state.created_at, state.changed_at, state.suspension_id, state.cancellation_id, state.sequence, state.version, state.seal, state.seal_key_id, state.seal_version FROM identity.accounts state";

    fn decode(_: Token, row: &Row, keys: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let id = row.try_get("id").map_err(Error::decode)?;
        let created_at = row.try_get("created_at").map_err(Error::decode)?;
        let changed_at = row.try_get("changed_at").map_err(Error::decode)?;
        if created_at > changed_at || changed_at > now {
            return Err(Error::Integrity(
                "account creation and its current state are not from the future",
            ));
        }
        let mobile: Option<Envelope> = row.try_get("mobile").map_err(Error::decode)?;
        let number: Option<Hash> = row.try_get("id_number").map_err(Error::decode)?;
        let suspension_id = row.try_get("suspension_id").map_err(Error::decode)?;
        let cancellation_id = row.try_get("cancellation_id").map_err(Error::decode)?;
        let status = match (cancellation_id, mobile, number, suspension_id) {
            (None, Some(mobile), Some(number), suspension_id) => Status::Current(Current {
                plaintext: open_mobile(&mobile, keys.data, id)?,
                mobile: Arc::new(mobile),
                number: Arc::new(number),
                suspension_id,
            }),
            (Some(id), None, None, None) => Status::Cancelled(id),
            _ => {
                return Err(Error::Integrity(
                    "current accounts retain both occupancies and tombstones retain neither",
                ));
            }
        };
        Ok(Self {
            data: Data {
                id,
                person_id: row.try_get("person_id").map_err(Error::decode)?,
                status,
                avatar: row.try_get("avatar").map_err(Error::decode)?,
                wechat: row.try_get("wechat").map_err(Error::decode)?,
                created_at,
                changed_at,
            },
            sequence: row.try_get("sequence").map_err(Error::decode)?,
            version: row.try_get("version").map_err(Error::decode)?,
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let (status, mobile, hash, number, suspension, cancellation) = match &self.data.status {
            Status::Current(current) => (
                b"current".to_vec(),
                current.plaintext.as_bytes().to_vec(),
                current.mobile.hash().as_ref().to_vec(),
                current.number.as_ref().as_ref().to_vec(),
                current
                    .suspension_id
                    .map(|id| id.as_bytes().to_vec())
                    .unwrap_or_default(),
                Vec::new(),
            ),
            Status::Cancelled(id) => (
                b"cancelled".to_vec(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                id.as_bytes().to_vec(),
            ),
        };
        Zeroizing::new(vec![
            self.data.id.as_bytes().to_vec(),
            self.data.person_id.as_bytes().to_vec(),
            status,
            mobile,
            hash,
            number,
            suspension,
            cancellation,
            self.data.avatar.as_bytes().to_vec(),
            self.data.wechat.as_bytes().to_vec(),
            self.data
                .created_at
                .timestamp_micros()
                .to_be_bytes()
                .to_vec(),
            self.data
                .changed_at
                .timestamp_micros()
                .to_be_bytes()
                .to_vec(),
            self.sequence.to_be_bytes().to_vec(),
            self.version.to_be_bytes().to_vec(),
        ])
    }
}

impl history::State for State {
    type Id = Id;
    type Data = Data;
    type Record = Record;

    fn build(_: Token, data: Data, sequence: i64, version: i64) -> Self {
        Self {
            data,
            sequence,
            version,
        }
    }

    fn id(&self) -> Id {
        self.data.id
    }

    fn bytes(id: Id) -> [u8; 16] {
        *id.as_bytes()
    }

    fn sequence(&self) -> i64 {
        self.sequence
    }

    fn version(&self) -> i64 {
        self.version
    }

    fn has_head(&self, record: &Record) -> bool {
        record.matches(self)
    }

    fn locators(&self) -> Vec<Locator> {
        let mut keys = vec![Locator::new::<Self>(
            "person",
            self.data.person_id.as_bytes(),
        )];
        if let Some(current) = self.current() {
            keys.push(Locator::new::<Self>(
                "mobile",
                current.mobile.hash().as_ref(),
            ));
            keys.push(Locator::new::<Self>(
                "number",
                current.number.as_ref().as_ref(),
            ));
        }
        keys
    }

    async fn write(
        &self,
        _: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> Result<u64, Error> {
        let current = self.current();
        let mobile = current.map(Current::mobile);
        let number = current.map(Current::number);
        let suspension_id = current.and_then(Current::suspension_id);
        let cancellation_id = self.cancellation_id();
        if let Some(original) = original {
            Ok(client.execute(
                "UPDATE identity.accounts SET mobile=$2, id_number=$3, avatar=$4, wechat=$5, changed_at=$6, suspension_id=$7, cancellation_id=$8, sequence=$9, version=$10, seal=$11, seal_key_id=$12, seal_version=$13 WHERE id=$1 AND version=$14 AND seal=$15 AND seal_key_id=$16 AND seal_version=$17",
                &[&self.data.id, &mobile, &number, &self.data.avatar, &self.data.wechat,
                  &self.data.changed_at, &suspension_id, &cancellation_id, &self.sequence,
                  &self.version, &seal.tag(), &seal.key_id(), &seal.version(),
                  &original.version, &original.seal().tag(), &original.seal().key_id(),
                  &original.seal().version()],
            ).await?)
        } else {
            Ok(client.execute(
                "INSERT INTO identity.accounts (id, person_id, mobile, id_number, avatar, wechat, created_at, changed_at, suspension_id, cancellation_id, sequence, version, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
                &[&self.data.id, &self.data.person_id, &mobile, &number,
                  &self.data.avatar, &self.data.wechat, &self.data.created_at,
                  &self.data.changed_at, &suspension_id, &cancellation_id,
                  &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version()],
            ).await?)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use chrono::DateTime;
    use platform::cryptography::sha256;
    use tokio_postgres::types::{Field, FromSql, Kind as SqlKind, Type};

    use super::{Current, Data, State, Status};
    use crate::{
        history::{self, Kind as _, Locator, Seed, State as _, Verified},
        identity::{keys::encryption, mobile::envelope::Binding},
        keys::{Derived, Entry, Id, Material, seal},
    };

    const MOBILE: &str = "010000000000000000000102030405060708090a0b264366e4f0b7f18d15add1de375ea74b4c7a5b8095e166c6ca48d0";
    const MOBILE_HASH: &str = "3845b46ad75387acdf91db39dbe9295ea6ee7abb99b1f77dfbad19a350d3eae6";
    const NUMBER: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";

    #[derive(Clone, Copy)]
    enum Shape {
        Live,
        Suspended,
        Cancelled,
    }

    fn id<T: for<'a> FromSql<'a>>(number: u8) -> T {
        let mut bytes = [0; 16];
        bytes[6] = 0x40;
        bytes[8] = 0x80;
        bytes[15] = number;
        T::from_sql(&Type::UUID, &bytes).expect("fixed UUID fixture")
    }

    fn derived() -> Derived {
        let id = Id::new(0).expect("key ID");
        Derived::fixed(
            id,
            vec![Entry::new(
                id,
                Material(zeroize::Zeroizing::new(std::array::from_fn(|index| {
                    u8::try_from(index).expect("32 key bytes")
                }))),
            )],
        )
    }

    fn state(shape: Shape, keys: history::Keys<'_>) -> Verified<State> {
        let status = match shape {
            Shape::Live | Shape::Suspended => {
                // A fixed PostgreSQL composite carries an independent Account AEAD vector.
                let kind = Type::new(
                    "mobile".into(),
                    12345,
                    SqlKind::Composite(vec![
                        Field::new("value".into(), Type::BYTEA),
                        Field::new("hash".into(), Type::BYTEA),
                    ]),
                    "identity".into(),
                );
                let bytes = hex::decode(concat!(
                    "000000020000001100000030",
                    "010000000000000000000102030405060708090a0b264366e4f0b7f18d15add1de375ea74b4c7a5b8095e166c6ca48d0",
                    "0000001100000020",
                    "3845b46ad75387acdf91db39dbe9295ea6ee7abb99b1f77dfbad19a350d3eae6",
                ))
                .expect("fixed composite bytes");
                let mobile = super::Envelope::from_sql(&kind, &bytes).expect("mobile composite");
                let lookup = crate::identity::keys::mobile::Keys::new(derived());
                assert_eq!(
                    mobile
                        .open(keys.data, &lookup, Binding::Account(id(1)))
                        .expect("Account-bound mobile vector")
                        .as_ref(),
                    "13800000000",
                );
                Status::Current(Current {
                    plaintext: super::open_mobile(&mobile, keys.data, id(1))
                        .expect("canonical Account mobile"),
                    mobile: std::sync::Arc::new(mobile),
                    number: std::sync::Arc::new(
                        super::Hash::from_sql(
                            &Type::BYTEA,
                            &hex::decode(NUMBER).expect("number digest bytes"),
                        )
                        .expect("number digest"),
                    ),
                    suspension_id: matches!(shape, Shape::Suspended).then(|| id(5)),
                })
            }
            Shape::Cancelled => Status::Cancelled(id(6)),
        };
        Seed::state::<State>(
            Data {
                id: id(1),
                person_id: id(2),
                status,
                avatar: id(3),
                wechat: id(4),
                created_at: DateTime::from_timestamp_micros(-1_234_567).expect("pre-epoch time"),
                changed_at: DateTime::from_timestamp_micros(1_700_000_000_123_456)
                    .expect("microsecond time"),
            },
            258,
            66051,
            keys,
        )
        .expect("sealed state fixture")
    }

    fn expected_fields(shape: Shape) -> Vec<Vec<u8>> {
        let (status, mobile, hash, number, suspension, cancellation) = match shape {
            Shape::Live => (
                "63757272656e74",
                "3133383030303030303030",
                MOBILE_HASH,
                NUMBER,
                "",
                "",
            ),
            Shape::Suspended => (
                "63757272656e74",
                "3133383030303030303030",
                MOBILE_HASH,
                NUMBER,
                "00000000000040008000000000000005",
                "",
            ),
            Shape::Cancelled => (
                "63616e63656c6c6564",
                "",
                "",
                "",
                "",
                "00000000000040008000000000000006",
            ),
        };
        [
            "00000000000040008000000000000001",
            "00000000000040008000000000000002",
            status,
            mobile,
            hash,
            number,
            suspension,
            cancellation,
            "00000000000040008000000000000003",
            "00000000000040008000000000000004",
            "ffffffffffed2979",
            "00060a2418202240",
            "0000000000000102",
            "0000000000010203",
        ]
        .into_iter()
        .map(|bytes| hex::decode(bytes).expect("literal field bytes"))
        .collect()
    }

    #[test]
    fn account_encodings_match_independent_python_vectors() {
        let encryption = encryption::Keys::new(derived());
        let seals = seal::Keys::new(derived());
        let keys = history::Keys {
            data: &encryption,
            seals: &seals,
        };
        // Python AESGCMSIV, UUID.bytes, struct.pack(">q" / ">I"), SHA-256 and HMAC.
        for (shape, length, digest, tag) in [
            (
                Shape::Live,
                278,
                "ff69b47ca0ff57b24fe70d8d39987a422801646d6c202af51926d0de82a4d7f9",
                "60227a1fbdb53d42cf05d30a42cccdf58010c09691379b387a29df06fa19565d",
            ),
            (
                Shape::Suspended,
                294,
                "614ece9f43a3c932ff63831eda96e8f7e66e921ed0266bce038feea40d2d7f9c",
                "dc456e8e02f9658a04d314bc3c1865ed66cbac89565fa670dff82de8dd491b1d",
            ),
            (
                Shape::Cancelled,
                221,
                "6250d50536a2acceaf0a58924928b76fc61e8f583bee0691f840bb13dc64838c",
                "211526cc773bf3fd1528508eca33bb25f12af13dbb8aa3c72071fcd24f336dcc",
            ),
        ] {
            let state = state(shape, keys);
            let fields = state.fields();
            assert_eq!(*fields, expected_fields(shape));
            let bytes = seal::encode_fields(
                State::NAME,
                State::VERSION,
                &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            )
            .expect("frozen Account encoding");
            assert_eq!(bytes.as_ref().len(), length);
            assert_eq!(hex::encode(sha256::Digest::new(&bytes).as_ref()), digest);
            assert_eq!(hex::encode(state.seal().tag()), tag);
        }
    }

    #[test]
    fn current_shapes_keep_occupancies_and_tombstones_release_them() {
        let encryption = encryption::Keys::new(derived());
        let seals = seal::Keys::new(derived());
        let keys = history::Keys {
            data: &encryption,
            seals: &seals,
        };
        for shape in [Shape::Live, Shape::Suspended, Shape::Cancelled] {
            let state = state(shape, keys);
            assert_eq!(state.id(), id(1));
            assert_eq!(state.person_id(), id(2));
            assert_eq!(state.avatar(), id(3));
            assert_eq!(state.wechat(), id(4));
            assert_eq!(state.created_at().timestamp_micros(), -1_234_567);
            assert_eq!(state.changed_at().timestamp_micros(), 1_700_000_000_123_456);
            assert_eq!(state.sequence(), 258);
            assert_eq!(state.version(), 66051);
            let mut locators = vec![Locator::new::<State>(
                "person",
                &hex::decode("00000000000040008000000000000002").expect("person bytes"),
            )];
            match shape {
                Shape::Live | Shape::Suspended => {
                    let current = state.current().expect("retained occupancies");
                    assert_eq!(hex::encode(current.mobile().ciphertext().as_ref()), MOBILE);
                    assert_eq!(hex::encode(current.mobile().hash().as_ref()), MOBILE_HASH);
                    assert_eq!(hex::encode(current.number().as_ref()), NUMBER);
                    assert_eq!(
                        current.suspension_id(),
                        matches!(shape, Shape::Suspended).then(|| id(5))
                    );
                    assert!(state.cancellation_id().is_none());
                    locators.push(Locator::new::<State>(
                        "mobile",
                        &hex::decode(MOBILE_HASH).expect("mobile digest bytes"),
                    ));
                    locators.push(Locator::new::<State>(
                        "number",
                        &hex::decode(NUMBER).expect("number digest bytes"),
                    ));
                }
                Shape::Cancelled => {
                    assert!(state.current().is_none());
                    assert_eq!(state.cancellation_id(), Some(id(6)));
                }
            }
            assert!(state.locators() == locators);
        }
    }

    #[test]
    fn mobile_reencryption_preserves_canonical_fields_and_seal() {
        let encryption = encryption::Keys::new(derived());
        let seals = seal::Keys::new(derived());
        let keys = history::Keys {
            data: &encryption,
            seals: &seals,
        };
        let lookup = crate::identity::keys::mobile::Keys::new(derived());
        for shape in [Shape::Live, Shape::Suspended] {
            let original = state(shape, keys);
            let mut data = original.data();
            let Status::Current(current) = &mut data.status else {
                panic!("current fixture");
            };
            let mobile = super::Envelope::seal(
                &encryption,
                &lookup,
                Binding::Account(id(1)),
                &current.plaintext,
            )
            .expect("fresh Account-bound envelope");
            assert_ne!(
                mobile.ciphertext().as_ref(),
                current.mobile.ciphertext().as_ref()
            );
            current.plaintext = super::open_mobile(&mobile, &encryption, id(1))
                .expect("re-encrypted canonical mobile");
            current.mobile = std::sync::Arc::new(mobile);
            let resealed = Seed::state::<State>(data, 258, 66051, keys)
                .expect("same semantic state with a fresh envelope");
            assert_eq!(*resealed.fields(), expected_fields(shape));
            assert_eq!(resealed.seal(), original.seal());
            assert!(resealed.locators() == original.locators());
        }
    }

    #[test]
    fn mobile_open_rejects_another_account_binding() {
        let encryption = encryption::Keys::new(derived());
        let seals = seal::Keys::new(derived());
        let keys = history::Keys {
            data: &encryption,
            seals: &seals,
        };
        let state = state(Shape::Live, keys);
        let mobile = state.current().expect("current fixture").mobile();
        assert!(matches!(
            super::open_mobile(mobile, &encryption, id(7)),
            Err(history::Error::Encryption(encryption::Error::Envelope(
                platform::cryptography::envelope::Error::Authentication(_)
            )))
        ));
    }
}
