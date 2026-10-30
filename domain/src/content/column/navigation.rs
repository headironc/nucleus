//! The singleton owns column order; each append records its creating column change.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    content::column::{Id, change},
    history::{self, Keys, Kind, Locator, State, Token, Verified},
    identity::operation,
    keys::seal::Seal,
};

pub(crate) const TABLE: &str = "content.navigations";
pub(crate) const RECORDS: &str = "content.appends";

pub(super) async fn verify_sources(client: &Client) -> Result<(), history::Error> {
    let valid: bool = client.query_one(
        "SELECT NOT EXISTS (SELECT 1 FROM content.appends append LEFT JOIN content.changes change ON change.id=append.change_id WHERE change.id IS NULL OR (change.column_id, change.operation_id, change.created_at, change.transaction_id, change.index) IS DISTINCT FROM (append.column_id, append.operation_id, append.created_at, append.transaction_id, append.sequence - 1)) AND NOT EXISTS (SELECT 1 FROM content.appends append WHERE append.sequence > 1 AND NOT EXISTS (SELECT 1 FROM content.appends previous WHERE previous.navigation_id=append.navigation_id AND previous.sequence=append.sequence - 1 AND previous.\"order\"=append.\"order\"[1:append.sequence - 1] AND previous.created_at <= append.created_at))",
        &[],
    ).await?.try_get(0).map_err(history::Error::decode)?;
    if !valid {
        return Err(history::Error::Integrity(
            "navigation appends retain their exact same-transaction creation and predecessor",
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub(super) struct Navigation {
    id: Uuid,
    order: Vec<Id>,
    append_id: Uuid,
    changed_at: DateTime<Utc>,
    sequence: i64,
    version: i64,
}

pub(super) struct Data {
    pub(super) id: Uuid,
    pub(super) order: Vec<Id>,
    pub(super) append_id: Uuid,
    pub(super) changed_at: DateTime<Utc>,
}

impl Navigation {
    pub(super) fn order(&self) -> &[Id] {
        &self.order
    }
    pub(super) fn changed_at(&self) -> DateTime<Utc> {
        self.changed_at
    }
}

fn order_bytes(order: &[Id]) -> Vec<u8> {
    order
        .iter()
        .flat_map(|id| id.as_bytes().iter().copied())
        .collect()
}

fn check_order(order: &[Id]) -> Result<(), history::Error> {
    if order.is_empty()
        || order.len() > 100
        || order.iter().collect::<std::collections::HashSet<_>>().len() != order.len()
    {
        return Err(history::Error::Integrity(
            "navigation contains 1 to 100 distinct column identities",
        ));
    }
    Ok(())
}

impl Kind for Navigation {
    type Error = history::Error;
    const NAME: &'static str = "content.navigation";
    const SELECT: &'static str = "SELECT state.id, state.singleton, state.order, state.append_id, state.changed_at, state.sequence, state.version, state.seal, state.seal_key_id, state.seal_version FROM content.navigations state";
    fn decode(
        _: Token,
        row: &Row,
        _: Keys<'_>,
        now: DateTime<Utc>,
    ) -> Result<Self, history::Error> {
        let singleton: bool = row.try_get("singleton").map_err(history::Error::decode)?;
        let value = Self {
            id: row.try_get("id").map_err(history::Error::decode)?,
            order: row.try_get("order").map_err(history::Error::decode)?,
            append_id: row.try_get("append_id").map_err(history::Error::decode)?,
            changed_at: row.try_get("changed_at").map_err(history::Error::decode)?,
            sequence: row.try_get("sequence").map_err(history::Error::decode)?,
            version: row.try_get("version").map_err(history::Error::decode)?,
        };
        check_order(&value.order)?;
        if !singleton || value.changed_at > now {
            return Err(history::Error::Integrity(
                "navigation is a singleton with non-future time",
            ));
        }
        Ok(value)
    }
    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        Zeroizing::new(vec![
            self.id.as_bytes().to_vec(),
            vec![1],
            order_bytes(&self.order),
            self.append_id.as_bytes().to_vec(),
            self.changed_at.timestamp_micros().to_string().into_bytes(),
            self.sequence.to_string().into_bytes(),
            self.version.to_string().into_bytes(),
        ])
    }
}

impl State for Navigation {
    type Id = Uuid;
    type Data = Data;
    type Record = Append;
    fn build(_: Token, data: Data, sequence: i64, version: i64) -> Self {
        Self {
            id: data.id,
            order: data.order,
            append_id: data.append_id,
            changed_at: data.changed_at,
            sequence,
            version,
        }
    }
    fn id(&self) -> Uuid {
        self.id
    }
    fn bytes(id: Uuid) -> [u8; 16] {
        *id.as_bytes()
    }
    fn sequence(&self) -> i64 {
        self.sequence
    }
    fn version(&self) -> i64 {
        self.version
    }
    fn has_head(&self, record: &Append) -> bool {
        self.append_id == record.id
            && self.order == record.order
            && self.changed_at == record.created_at
    }
    fn locators(&self) -> Vec<Locator> {
        vec![Locator::new::<Self>("singleton", b"columns")]
    }

    async fn write(
        &self,
        _: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> Result<u64, history::Error> {
        Ok(match original {
            None => client.execute("INSERT INTO content.navigations (id, singleton, \"order\", append_id, changed_at, sequence, version, seal, seal_key_id, seal_version) VALUES ($1,true,$2,$3,$4,$5,$6,$7,$8,$9)",
                &[&self.id, &self.order, &self.append_id, &self.changed_at, &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version()]).await?,
            Some(original) => client.execute("UPDATE content.navigations SET \"order\"=$2, append_id=$3, changed_at=$4, sequence=$5, version=$6, seal=$7, seal_key_id=$8, seal_version=$9 WHERE id=$1 AND version=$10 AND seal=$11 AND seal_key_id=$12 AND seal_version=$13",
                &[&self.id, &self.order, &self.append_id, &self.changed_at, &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version(), &original.version(), &original.seal().tag(), &original.seal().key_id(), &original.seal().version()]).await?,
        })
    }
}

pub(super) struct Append {
    id: Uuid,
    navigation_id: Uuid,
    sequence: i64,
    column_id: Id,
    change_id: change::Id,
    order: Vec<Id>,
    created_at: DateTime<Utc>,
    operation_id: operation::Id,
}

pub(super) struct AppendData {
    pub(super) id: Uuid,
    pub(super) column_id: Id,
    pub(super) change_id: change::Id,
    pub(super) order: Vec<Id>,
    pub(super) created_at: DateTime<Utc>,
    pub(super) operation_id: operation::Id,
}

impl Append {
    pub(super) fn change_id(&self) -> change::Id {
        self.change_id
    }
    pub(super) fn column_id(&self) -> Id {
        self.column_id
    }
    pub(super) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Kind for Append {
    type Error = history::Error;
    const NAME: &'static str = "content.navigation.append";
    const SELECT: &'static str = "SELECT record.id, record.navigation_id, record.sequence, record.column_id, record.change_id, record.order, record.created_at, record.operation_id, record.seal, record.seal_key_id, record.seal_version FROM content.appends record";
    fn decode(
        _: Token,
        row: &Row,
        _: Keys<'_>,
        now: DateTime<Utc>,
    ) -> Result<Self, history::Error> {
        let value = Self {
            id: row.try_get("id").map_err(history::Error::decode)?,
            navigation_id: row
                .try_get("navigation_id")
                .map_err(history::Error::decode)?,
            sequence: row.try_get("sequence").map_err(history::Error::decode)?,
            column_id: row.try_get("column_id").map_err(history::Error::decode)?,
            change_id: row.try_get("change_id").map_err(history::Error::decode)?,
            order: row.try_get("order").map_err(history::Error::decode)?,
            created_at: row.try_get("created_at").map_err(history::Error::decode)?,
            operation_id: row
                .try_get("operation_id")
                .map_err(history::Error::decode)?,
        };
        check_order(&value.order)?;
        if value.created_at > now
            || value.sequence != value.order.len() as i64
            || value.order.last() != Some(&value.column_id)
        {
            return Err(history::Error::Integrity(
                "a navigation append has a positive sequence and non-future time",
            ));
        }
        Ok(value)
    }
    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        Zeroizing::new(vec![
            self.id.as_bytes().to_vec(),
            self.navigation_id.as_bytes().to_vec(),
            self.sequence.to_string().into_bytes(),
            self.column_id.as_bytes().to_vec(),
            self.change_id.as_bytes().to_vec(),
            order_bytes(&self.order),
            self.created_at.timestamp_micros().to_string().into_bytes(),
            self.operation_id.as_bytes().to_vec(),
        ])
    }
}

impl history::Record for Append {
    const OBJECT: &'static str = "navigation_id";
    type Object = Uuid;
    type Data = AppendData;
    fn build(_: Token, navigation_id: Uuid, sequence: i64, data: AppendData) -> Self {
        Self {
            id: data.id,
            navigation_id,
            sequence,
            column_id: data.column_id,
            change_id: data.change_id,
            order: data.order,
            created_at: data.created_at,
            operation_id: data.operation_id,
        }
    }
    fn object(&self) -> Uuid {
        self.navigation_id
    }
    fn sequence(&self) -> i64 {
        self.sequence
    }
    fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
    async fn insert(
        &self,
        _: Token,
        client: &Client,
        _: Keys<'_>,
        seal: &Seal,
    ) -> Result<(), history::Error> {
        client.execute("INSERT INTO content.appends (id, navigation_id, sequence, column_id, change_id, \"order\", created_at, operation_id, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
            &[&self.id, &self.navigation_id, &self.sequence, &self.column_id, &self.change_id, &self.order, &self.created_at, &self.operation_id, &seal.tag(), &seal.key_id(), &seal.version()]).await?;
        Ok(())
    }
}
