//! Permanent column creation facts and their original results.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::{
    Client, Row,
    types::{FromSql, ToSql},
};
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    content::column::{self, Column, Creation, Error},
    history::{self, Keys, Kind, Token},
    identity::{operation, person::revision},
    keys::seal::Seal,
};

pub(crate) const TABLE: &str = "content.changes";

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    ToSchema,
    FromSql,
    ToSql,
)]
#[serde(transparent)]
#[schema(as = column::change::Id)]
#[postgres(transparent)]
pub struct Id(Uuid);

impl Id {
    pub(super) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(super) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

pub struct Change {
    pub(super) id: Id,
    pub(super) column: Column,
    pub(super) operator: revision::Identity,
    pub(super) created_at: DateTime<Utc>,
    pub(super) operation_id: operation::Id,
}

impl Change {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn column(&self) -> &Column {
        &self.column
    }
    pub fn operator(&self) -> &revision::Identity {
        &self.operator
    }
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
}

pub(super) struct Record {
    id: Id,
    column_id: column::Id,
    sequence: i64,
    input: Creation,
    index: i32,
    operator_id: revision::Id,
    created_at: DateTime<Utc>,
    operation_id: operation::Id,
    digest: Vec<u8>,
}

pub(super) struct Data {
    pub(super) id: Id,
    pub(super) input: Creation,
    pub(super) index: i32,
    pub(super) operator_id: revision::Id,
    pub(super) created_at: DateTime<Utc>,
    pub(super) operation_id: operation::Id,
    pub(super) digest: Vec<u8>,
}

impl Record {
    pub(super) fn id(&self) -> Id {
        self.id
    }
    pub(super) fn column_id(&self) -> column::Id {
        self.column_id
    }
    pub(super) fn input(&self) -> &Creation {
        &self.input
    }
    pub(super) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub(super) fn operator_id(&self) -> revision::Id {
        self.operator_id
    }
    pub(super) fn digest(&self) -> &[u8] {
        &self.digest
    }
    pub(super) fn column(&self) -> Column {
        Column {
            id: self.column_id,
            key: self.input.key.clone(),
            name: self.input.name.clone(),
            description: self.input.description.clone(),
            enabled: self.input.enabled,
            index: self.index,
            change_id: self.id,
        }
    }
    pub(super) fn result(&self, operator: revision::Identity) -> Change {
        Change {
            id: self.id,
            column: self.column(),
            operator,
            created_at: self.created_at,
            operation_id: self.operation_id,
        }
    }
}

impl Kind for Record {
    type Error = history::Error;
    const NAME: &'static str = "content.column.change";
    const SELECT: &'static str = "SELECT record.id, record.column_id, record.sequence, record.key, record.name, record.description, record.enabled, record.index, record.operator_id, record.created_at, record.operation_id, record.digest, record.seal, record.seal_key_id, record.seal_version FROM content.changes record";

    fn decode(
        _: Token,
        row: &Row,
        _: Keys<'_>,
        now: DateTime<Utc>,
    ) -> Result<Self, history::Error> {
        let value = Self {
            id: row.try_get("id").map_err(history::Error::decode)?,
            column_id: row.try_get("column_id").map_err(history::Error::decode)?,
            sequence: row.try_get("sequence").map_err(history::Error::decode)?,
            input: Creation {
                key: row.try_get("key").map_err(history::Error::decode)?,
                name: row.try_get("name").map_err(history::Error::decode)?,
                description: row.try_get("description").map_err(history::Error::decode)?,
                enabled: row.try_get("enabled").map_err(history::Error::decode)?,
            },
            index: row.try_get("index").map_err(history::Error::decode)?,
            operator_id: row.try_get("operator_id").map_err(history::Error::decode)?,
            created_at: row.try_get("created_at").map_err(history::Error::decode)?,
            operation_id: row
                .try_get("operation_id")
                .map_err(history::Error::decode)?,
            digest: row.try_get("digest").map_err(history::Error::decode)?,
        };
        if value.created_at > now
            || value.sequence != 1
            || !(0..100).contains(&value.index)
            || value.digest.len() != 32
        {
            return Err(history::Error::Integrity(
                "a column creation has its initial sequence, bounded index, digest and non-future time",
            ));
        }
        Ok(value)
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        Zeroizing::new(vec![
            self.id.as_bytes().to_vec(),
            self.column_id.as_bytes().to_vec(),
            self.sequence.to_string().into_bytes(),
            self.input.key.as_str().as_bytes().to_vec(),
            self.input.name.as_str().as_bytes().to_vec(),
            self.input.description.as_str().as_bytes().to_vec(),
            vec![u8::from(self.input.enabled)],
            self.index.to_string().into_bytes(),
            self.operator_id.as_bytes().to_vec(),
            self.created_at.timestamp_micros().to_string().into_bytes(),
            self.operation_id.as_bytes().to_vec(),
            self.digest.clone(),
        ])
    }
}

impl history::Record for Record {
    const OBJECT: &'static str = "column_id";
    type Object = column::Id;
    type Data = Data;
    fn build(_: Token, column_id: column::Id, sequence: i64, data: Data) -> Self {
        Self {
            id: data.id,
            column_id,
            sequence,
            input: data.input,
            index: data.index,
            operator_id: data.operator_id,
            created_at: data.created_at,
            operation_id: data.operation_id,
            digest: data.digest,
        }
    }
    fn object(&self) -> column::Id {
        self.column_id
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
        client.execute("INSERT INTO content.changes (id, column_id, sequence, key, name, description, enabled, index, operator_id, created_at, operation_id, digest, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
            &[&self.id, &self.column_id, &self.sequence, &self.input.key, &self.input.name, &self.input.description, &self.input.enabled, &self.index, &self.operator_id, &self.created_at, &self.operation_id, &self.digest, &seal.tag(), &seal.key_id(), &seal.version()]).await?;
        Ok(())
    }
}

pub(super) async fn original(
    client: &Client,
    service: &column::Service,
    operation: operation::Id,
    now: DateTime<Utc>,
) -> Result<history::Verified<Record>, Error> {
    history::original::<Record>(client, service.app.keys(), operation, now)
        .await?
        .ok_or(Error::corrupt(
            "a column completion retains its original creation",
        ))
}
