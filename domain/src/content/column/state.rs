//! Permanent column identity and current values. Its key is never released.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use zeroize::Zeroizing;

use crate::{
    content::column::{Creation, Id, change},
    history::{self, Keys, Kind, Locator, State, Token, Verified},
    keys::seal::Seal,
};

pub(crate) const TABLE: &str = "content.columns";

#[derive(Clone)]
pub(super) struct ColumnState {
    id: Id,
    input: Creation,
    change_id: change::Id,
    created_at: DateTime<Utc>,
    sequence: i64,
    version: i64,
}

pub(super) struct Data {
    pub(super) id: Id,
    pub(super) input: Creation,
    pub(super) change_id: change::Id,
    pub(super) created_at: DateTime<Utc>,
}

impl ColumnState {
    pub(super) fn input(&self) -> &Creation {
        &self.input
    }
}

impl Kind for ColumnState {
    type Error = history::Error;
    const NAME: &'static str = "content.column";
    const SELECT: &'static str = "SELECT state.id, state.key, state.name, state.description, state.enabled, state.change_id, state.created_at, state.sequence, state.version, state.seal, state.seal_key_id, state.seal_version FROM content.columns state";

    fn decode(
        _: Token,
        row: &Row,
        _: Keys<'_>,
        now: DateTime<Utc>,
    ) -> Result<Self, history::Error> {
        let value = Self {
            id: row.try_get("id").map_err(history::Error::decode)?,
            input: Creation {
                key: row.try_get("key").map_err(history::Error::decode)?,
                name: row.try_get("name").map_err(history::Error::decode)?,
                description: row.try_get("description").map_err(history::Error::decode)?,
                enabled: row.try_get("enabled").map_err(history::Error::decode)?,
            },
            change_id: row.try_get("change_id").map_err(history::Error::decode)?,
            created_at: row.try_get("created_at").map_err(history::Error::decode)?,
            sequence: row.try_get("sequence").map_err(history::Error::decode)?,
            version: row.try_get("version").map_err(history::Error::decode)?,
        };
        if value.created_at > now || value.sequence != 1 || value.version != 1 {
            return Err(history::Error::Integrity(
                "a created column retains its first sealed state and non-future time",
            ));
        }
        Ok(value)
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        Zeroizing::new(vec![
            self.id.as_bytes().to_vec(),
            self.input.key.as_str().as_bytes().to_vec(),
            self.input.name.as_str().as_bytes().to_vec(),
            self.input.description.as_str().as_bytes().to_vec(),
            vec![u8::from(self.input.enabled)],
            self.change_id.as_bytes().to_vec(),
            self.created_at.timestamp_micros().to_string().into_bytes(),
            self.sequence.to_string().into_bytes(),
            self.version.to_string().into_bytes(),
        ])
    }
}

impl State for ColumnState {
    type Id = Id;
    type Data = Data;
    type Record = change::Record;
    fn build(_: Token, data: Data, sequence: i64, version: i64) -> Self {
        Self {
            id: data.id,
            input: data.input,
            change_id: data.change_id,
            created_at: data.created_at,
            sequence,
            version,
        }
    }
    fn id(&self) -> Id {
        self.id
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
    fn has_head(&self, record: &change::Record) -> bool {
        record.id() == self.change_id
            && record.input() == &self.input
            && record.created_at() == self.created_at
    }
    fn locators(&self) -> Vec<Locator> {
        vec![
            Locator::new::<Self>("key", self.input.key.as_str().as_bytes()),
            Locator::new::<Self>("name", self.input.name.as_str().as_bytes()),
        ]
    }
    async fn write(
        &self,
        _: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> Result<u64, history::Error> {
        if original.is_some() {
            return Err(history::Error::Integrity(
                "this column consumer only creates permanent identities",
            ));
        }
        Ok(client.execute("INSERT INTO content.columns (id, key, name, description, enabled, change_id, created_at, sequence, version, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
            &[&self.id, &self.input.key, &self.input.name, &self.input.description, &self.input.enabled, &self.change_id, &self.created_at, &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version()]).await?)
    }
}
