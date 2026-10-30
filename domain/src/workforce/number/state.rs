//! A person's permanent number state, including revoked tombstones and authenticated pool slots.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    history::{self, Error, Keys, Kind, Locator, Read, Token, Verified},
    identity::person,
    keys::seal::Seal,
    workforce::number::{Id, Number, Revision, record::Record},
};

pub(crate) const TABLE: &str = "workforce.numbers";

#[derive(Clone)]
pub(super) struct Data {
    pub(super) id: person::Id,
    pub(super) revision: Revision,
    pub(super) held: bool,
    pub(super) tenure_id: Uuid,
    pub(super) changed_at: DateTime<Utc>,
}

#[derive(Clone)]
pub(super) struct State {
    data: Data,
    sequence: i64,
    version: i64,
}

impl State {
    pub(super) fn id(&self) -> person::Id {
        self.data.id
    }
    pub(super) fn revision(&self) -> Revision {
        self.data.revision
    }
    pub(super) fn current(&self) -> Option<Revision> {
        self.data.held.then_some(self.data.revision)
    }
    pub(super) fn held(&self) -> bool {
        self.data.held
    }
    pub(super) fn tenure_id(&self) -> Uuid {
        self.data.tenure_id
    }
    pub(super) fn changed_at(&self) -> DateTime<Utc> {
        self.data.changed_at
    }
}

pub(super) fn number(row: &Row) -> Result<Number, Error> {
    let subtype: i16 = row.try_get("subtype").map_err(Error::decode)?;
    let subtype = subtype
        .try_into()
        .map_err(|_| Error::Integrity("stored number subtype is valid"))?;
    Number::new(
        subtype,
        row.try_get("year").map_err(Error::decode)?,
        row.try_get("serial").map_err(Error::decode)?,
    )
    .map_err(|_| Error::Integrity("stored number year and serial are valid"))
}

impl Kind for State {
    type Error = Error;
    const NAME: &'static str = "workforce.number";
    const SELECT: &'static str = "SELECT state.id,state.revision_id,(state.number).subtype,(state.number).year,(state.number).serial,state.held,state.tenure_id,state.changed_at,state.sequence,state.version,state.seal,state.seal_key_id,state.seal_version FROM workforce.numbers state";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let changed_at = row.try_get("changed_at").map_err(Error::decode)?;
        if changed_at > now {
            return Err(Error::Integrity("number state time is not in the future"));
        }
        Ok(Self {
            data: Data {
                id: row.try_get("id").map_err(Error::decode)?,
                revision: Revision {
                    id: row.try_get("revision_id").map_err(Error::decode)?,
                    number: number(row)?,
                },
                held: row.try_get("held").map_err(Error::decode)?,
                tenure_id: row.try_get("tenure_id").map_err(Error::decode)?,
                changed_at,
            },
            sequence: row.try_get("sequence").map_err(Error::decode)?,
            version: row.try_get("version").map_err(Error::decode)?,
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let number = self.revision().number;
        Zeroizing::new(vec![
            self.id().as_bytes().to_vec(),
            self.revision().id.as_bytes().to_vec(),
            i16::from(number.subtype()).to_string().into_bytes(),
            number.year().to_string().into_bytes(),
            number.serial().to_string().into_bytes(),
            self.held().to_string().into_bytes(),
            self.tenure_id().as_bytes().to_vec(),
            self.changed_at()
                .timestamp_micros()
                .to_string()
                .into_bytes(),
            self.sequence.to_string().into_bytes(),
            self.version.to_string().into_bytes(),
        ])
    }
}

impl history::State for State {
    type Id = person::Id;
    type Data = Data;
    type Record = Record;

    fn build(_: Token, data: Data, sequence: i64, version: i64) -> Self {
        Self {
            data,
            sequence,
            version,
        }
    }
    fn id(&self) -> person::Id {
        self.id()
    }
    fn bytes(id: person::Id) -> [u8; 16] {
        *id.as_bytes()
    }
    fn sequence(&self) -> i64 {
        self.sequence
    }
    fn version(&self) -> i64 {
        self.version
    }
    fn has_head(&self, record: &Record) -> bool {
        record.person_id() == self.id()
            && record.number_id() == self.revision().id
            && record.held() == self.held()
            && record.tenure_id() == self.tenure_id()
            && record.time() == self.changed_at()
    }
    fn locators(&self) -> Vec<Locator> {
        if self.held() {
            vec![location(self.revision().number)]
        } else {
            Vec::new()
        }
    }
    async fn write(
        &self,
        _: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> Result<u64, Error> {
        let number = self.revision().number;
        let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[
            &self.id(),
            &self.revision().id,
            &i16::from(number.subtype()),
            &number.year(),
            &number.serial(),
            &self.held(),
            &self.tenure_id(),
            &self.changed_at(),
            &self.sequence,
            &self.version,
            &seal.tag(),
            &seal.key_id(),
            &seal.version(),
        ];
        Ok(match original {
            None => client.execute("INSERT INTO workforce.numbers (id,revision_id,number,held,tenure_id,changed_at,sequence,version,seal,seal_key_id,seal_version) VALUES ($1,$2,ROW($3::smallint,$4::integer,$5::smallint)::workforce.number,$6,$7,$8,$9,$10,$11,$12,$13)", params).await?,
            Some(original) => {
                let mut params = params.to_vec();
                let version = original.version;
                let tag = original.seal().tag();
                params.push(&version);
                params.push(&tag);
                client.execute("UPDATE workforce.numbers SET revision_id=$2,number=ROW($3::smallint,$4::integer,$5::smallint)::workforce.number,held=$6,tenure_id=$7,changed_at=$8,sequence=$9,version=$10,seal=$11,seal_key_id=$12,seal_version=$13 WHERE id=$1 AND version=$14 AND seal=$15", &params).await?
            }
        })
    }
}

fn pool_bytes(category: i32, year: i32) -> Vec<u8> {
    [
        category.to_be_bytes().as_slice(),
        year.to_be_bytes().as_slice(),
    ]
    .concat()
}

fn location(number: Number) -> Locator {
    let mut bytes = pool_bytes(number.subtype().category(), number.year());
    bytes.extend_from_slice(&number.serial().to_be_bytes());
    Locator::new::<State>("pool", &bytes)
}

pub(super) async fn pool(
    client: &Client,
    keys: Keys<'_>,
    anchors: &history::Anchors,
    read: Read,
    category: i32,
    year: i32,
    now: DateTime<Utc>,
) -> Result<Vec<Verified<State>>, Error> {
    let rows = client.query(&format!("{} WHERE state.held AND (state.number).subtype / 100 = $1 AND (state.number).year = $2 ORDER BY state.id", State::SELECT), &[&category, &year]).await?;
    history::related::<State>(
        &rows,
        &Locator::new::<State>("pool", &pool_bytes(category, year)),
        keys,
        anchors,
        read,
        now,
    )
}

pub(super) async fn holder(
    client: &Client,
    keys: Keys<'_>,
    anchors: &history::Anchors,
    read: Read,
    number: Number,
    now: DateTime<Utc>,
) -> Result<Option<Verified<State>>, Error> {
    let row = client.query_opt(&format!("{} WHERE state.held AND (state.number).subtype / 100 = $1 AND (state.number).year = $2 AND (state.number).serial = $3", State::SELECT), &[&number.subtype().category(), &number.year(), &number.serial()]).await?;
    history::located::<State>(row.as_ref(), &location(number), keys, anchors, read, now)
}

/// A discovery read only selects locks. The command authenticates the state after all locks.
pub(super) async fn discover(
    client: &Client,
    id: person::Id,
) -> Result<Option<(Id, Number)>, Error> {
    client.query_opt("SELECT revision_id,(number).subtype,(number).year,(number).serial FROM workforce.numbers WHERE id=$1", &[&id]).await?
        .map(|row| Ok((row.try_get("revision_id").map_err(Error::decode)?, number(&row)?))).transpose()
}
