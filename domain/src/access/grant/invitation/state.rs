//! One invitation's immutable creation, current terms and cumulative use.

use std::sync::Arc;

pub(crate) const TABLE: &str = "access.invitations";

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use zeroize::Zeroizing;

use crate::{
    access::{
        grant::{
            self,
            invitation::{Capacity, Code, Expiry, Id},
        },
        role,
    },
    database::sealed::Sealed,
    history::{self, Error, Keys, Kind, Locator, State as _, Token, Verified},
    identity::{
        operation,
        person::{self, revision},
    },
    keys::seal::Seal,
};

/// The permanent facts fixed when the invitation was created.
#[derive(Clone)]
pub(super) struct Creation {
    pub(super) id: Id,
    pub(super) person_id: person::Id,
    pub(super) grant_id: grant::Id,
    pub(super) epoch: i64,
    pub(super) role_id: role::Id,
    pub(super) change_id: role::change::Id,
    pub(super) operator_id: revision::Id,
    pub(super) capacity: Capacity,
    pub(super) expires_at: Expiry,
    pub(super) code: Arc<Sealed>,
    pub(super) plaintext: Arc<Code>,
    pub(super) hash: [u8; 32],
    pub(super) key_id: crate::keys::Id,
    pub(super) created_at: DateTime<Utc>,
    pub(super) operation_id: operation::Id,
}

/// The current projection every record of this object advances.
#[derive(Clone)]
pub(crate) struct Data {
    pub(super) creation: Creation,
    pub(super) capacity: Capacity,
    pub(super) expires_at: Expiry,
    pub(super) used: i64,
    pub(super) withdrawn_at: Option<DateTime<Utc>>,
    pub(super) changed_at: DateTime<Utc>,
}

#[derive(Clone)]
pub(crate) struct State {
    data: Data,
    sequence: i64,
    version: i64,
}

impl Data {
    pub(super) fn created(creation: Creation) -> Self {
        Self {
            capacity: creation.capacity,
            expires_at: creation.expires_at,
            used: 0,
            withdrawn_at: None,
            changed_at: creation.created_at,
            creation,
        }
    }
}

impl State {
    pub(in crate::access) fn data(&self) -> &Data {
        &self.data
    }
    pub(in crate::access) fn id(&self) -> Id {
        self.data.creation.id
    }
    pub(in crate::access) fn person_id(&self) -> person::Id {
        self.data.creation.person_id
    }
    pub(in crate::access) fn grant_id(&self) -> grant::Id {
        self.data.creation.grant_id
    }
    pub(in crate::access) fn epoch(&self) -> i64 {
        self.data.creation.epoch
    }
    pub(in crate::access) fn role_id(&self) -> role::Id {
        self.data.creation.role_id
    }
    pub(in crate::access) fn change_id(&self) -> role::change::Id {
        self.data.creation.change_id
    }
    pub(in crate::access) fn operator_id(&self) -> revision::Id {
        self.data.creation.operator_id
    }
    pub(in crate::access) fn original_capacity(&self) -> Capacity {
        self.data.creation.capacity
    }
    pub(in crate::access) fn original_expires_at(&self) -> Expiry {
        self.data.creation.expires_at
    }
    pub(in crate::access) fn capacity(&self) -> Capacity {
        self.data.capacity
    }
    pub(in crate::access) fn expires_at(&self) -> Expiry {
        self.data.expires_at
    }
    pub(in crate::access) fn used(&self) -> i64 {
        self.data.used
    }
    pub(in crate::access) fn withdrawn_at(&self) -> Option<DateTime<Utc>> {
        self.data.withdrawn_at
    }
    pub(in crate::access) fn created_at(&self) -> DateTime<Utc> {
        self.data.creation.created_at
    }
    pub(in crate::access) fn changed_at(&self) -> DateTime<Utc> {
        self.data.changed_at
    }
    pub(in crate::access) fn operation_id(&self) -> operation::Id {
        self.data.creation.operation_id
    }
    pub(in crate::access) fn code(&self) -> &Sealed {
        &self.data.creation.code
    }
    pub(in crate::access) fn hash(&self) -> &[u8; 32] {
        &self.data.creation.hash
    }
    pub(in crate::access) fn key_id(&self) -> crate::keys::Id {
        self.data.creation.key_id
    }
    pub(in crate::access) fn expired(&self, now: DateTime<Utc>) -> bool {
        DateTime::<Utc>::from(self.data.expires_at) <= now
    }

    /// A later event advances the same object at a nondecreasing trusted time.
    pub(super) fn next(&self, now: DateTime<Utc>) -> Result<Data, Error> {
        if self.data.withdrawn_at.is_some() || now < self.data.changed_at {
            return Err(Error::Integrity(
                "an unwithdrawn invitation advances at a nondecreasing time",
            ));
        }
        let mut data = self.data.clone();
        data.changed_at = now;
        Ok(data)
    }
}

impl Kind for State {
    type Error = Error;
    const NAME: &'static str = "access.invitation";
    const SELECT: &'static str = "SELECT state.id,state.domain,state.person_id,state.grant_id,state.epoch,state.role_id,state.change_id,state.operator_id,state.capacity,state.expires_at,state.code,state.hash,state.key_id,state.created_at,state.operation_id,state.current_capacity,state.current_expiry,state.used,state.withdrawn_at,state.changed_at,state.sequence,state.version,state.seal,state.seal_key_id,state.seal_version FROM access.invitations state";

    fn decode(_: Token, row: &Row, keys: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let created_at: DateTime<Utc> = row.try_get("created_at").map_err(Error::decode)?;
        let changed_at: DateTime<Utc> = row.try_get("changed_at").map_err(Error::decode)?;
        let withdrawn_at: Option<DateTime<Utc>> =
            row.try_get("withdrawn_at").map_err(Error::decode)?;
        let epoch: i64 = row.try_get("epoch").map_err(Error::decode)?;
        let used: i64 = row.try_get("used").map_err(Error::decode)?;
        let hash: Vec<u8> = row.try_get("hash").map_err(Error::decode)?;
        let hash: [u8; 32] = hash
            .try_into()
            .map_err(|_| Error::Integrity("an invitation retains a 32-byte lookup digest"))?;
        if row.try_get::<_, &str>("domain").map_err(Error::decode)? != "workforce"
            || created_at > changed_at
            || changed_at > now
            || epoch < 0
            || used < 0
            || withdrawn_at
                .is_some_and(|withdrawn_at| withdrawn_at < created_at || withdrawn_at != changed_at)
        {
            return Err(Error::Integrity(
                "an invitation retains its declared scope, ordered times and nonnegative counters",
            ));
        }
        let capacity: Capacity = row.try_get("capacity").map_err(Error::decode)?;
        let expires_at: Expiry = row.try_get("expires_at").map_err(Error::decode)?;
        let current_capacity: Capacity = row.try_get("current_capacity").map_err(Error::decode)?;
        let current_expiry: Expiry = row.try_get("current_expiry").map_err(Error::decode)?;
        if DateTime::<Utc>::from(expires_at) <= created_at || i64::from(current_capacity) < used {
            return Err(Error::Integrity(
                "an invitation retains a future original expiry and current capacity above use",
            ));
        }
        let id = row.try_get("id").map_err(Error::decode)?;
        let grant_id = row.try_get("grant_id").map_err(Error::decode)?;
        let code: Sealed = row.try_get("code").map_err(Error::decode)?;
        let plaintext = Code::open_history(keys.data, id, grant_id, epoch, &code)?;
        Ok(Self {
            data: Data {
                creation: Creation {
                    id,
                    person_id: row.try_get("person_id").map_err(Error::decode)?,
                    grant_id,
                    epoch,
                    role_id: row.try_get("role_id").map_err(Error::decode)?,
                    change_id: row.try_get("change_id").map_err(Error::decode)?,
                    operator_id: row.try_get("operator_id").map_err(Error::decode)?,
                    capacity,
                    expires_at,
                    code: Arc::new(code),
                    plaintext: Arc::new(plaintext),
                    hash,
                    key_id: row.try_get("key_id").map_err(Error::decode)?,
                    created_at,
                    operation_id: row.try_get("operation_id").map_err(Error::decode)?,
                },
                capacity: current_capacity,
                expires_at: current_expiry,
                used,
                withdrawn_at,
                changed_at,
            },
            sequence: row.try_get("sequence").map_err(Error::decode)?,
            version: row.try_get("version").map_err(Error::decode)?,
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let key_id: i64 = *self.key_id().as_ref();
        Zeroizing::new(vec![
            self.id().as_bytes().to_vec(),
            b"workforce".to_vec(),
            self.person_id().as_bytes().to_vec(),
            self.grant_id().as_bytes().to_vec(),
            self.epoch().to_string().into_bytes(),
            self.role_id().as_bytes().to_vec(),
            self.change_id().as_bytes().to_vec(),
            self.operator_id().as_bytes().to_vec(),
            i64::from(self.original_capacity()).to_string().into_bytes(),
            DateTime::<Utc>::from(self.original_expires_at())
                .timestamp_micros()
                .to_string()
                .into_bytes(),
            self.data.creation.plaintext.bytes().to_vec(),
            self.hash().to_vec(),
            key_id.to_string().into_bytes(),
            self.created_at()
                .timestamp_micros()
                .to_string()
                .into_bytes(),
            self.operation_id().as_bytes().to_vec(),
            i64::from(self.capacity()).to_string().into_bytes(),
            DateTime::<Utc>::from(self.expires_at())
                .timestamp_micros()
                .to_string()
                .into_bytes(),
            self.used().to_string().into_bytes(),
            self.withdrawn_at()
                .map(|at| at.timestamp_micros().to_string().into_bytes())
                .unwrap_or_default(),
            self.changed_at()
                .timestamp_micros()
                .to_string()
                .into_bytes(),
            self.sequence().to_string().into_bytes(),
            self.version().to_string().into_bytes(),
        ])
    }
}

impl history::State for State {
    type Id = Id;
    type Data = Data;
    type Record = super::record::Record;

    fn build(_: Token, data: Data, sequence: i64, version: i64) -> Self {
        Self {
            data,
            sequence,
            version,
        }
    }
    fn id(&self) -> Id {
        self.id()
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
    fn has_head(&self, record: &Self::Record) -> bool {
        record.matches(self)
    }
    fn locators(&self) -> Vec<Locator> {
        let key_id: i64 = *self.key_id().as_ref();
        let mut code = Vec::with_capacity(40);
        code.extend_from_slice(&key_id.to_be_bytes());
        code.extend_from_slice(self.hash());
        vec![
            Locator::new::<Self>(
                "history",
                &[
                    self.person_id().as_bytes().as_slice(),
                    self.id().as_bytes().as_slice(),
                ]
                .concat(),
            ),
            Locator::new::<Self>(
                "role",
                &[
                    self.role_id().as_bytes().as_slice(),
                    self.id().as_bytes().as_slice(),
                ]
                .concat(),
            ),
            Locator::new::<Self>("code", &code),
            Locator::new::<Self>("operation", self.operation_id().as_bytes()),
        ]
    }

    async fn write(
        &self,
        _: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> Result<u64, Error> {
        if let Some(original) = original {
            Ok(client
                .execute(
                    "UPDATE access.invitations SET current_capacity=$2,current_expiry=$3,used=$4,withdrawn_at=$5,changed_at=$6,sequence=$7,version=$8,seal=$9,seal_key_id=$10,seal_version=$11 WHERE id=$1 AND version=$12 AND seal=$13 AND seal_key_id=$14 AND seal_version=$15",
                    &[&self.id(), &self.capacity(), &self.expires_at(), &self.used(), &self.withdrawn_at(), &self.changed_at(), &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version(), &original.version(), &original.seal().tag(), &original.seal().key_id(), &original.seal().version()],
                )
                .await?)
        } else {
            Ok(client
                .execute(
                    "INSERT INTO access.invitations (id,person_id,grant_id,epoch,role_id,change_id,operator_id,capacity,expires_at,code,hash,key_id,created_at,operation_id,current_capacity,current_expiry,used,withdrawn_at,changed_at,sequence,version,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24)",
                    &[&self.id(), &self.person_id(), &self.grant_id(), &self.epoch(), &self.role_id(), &self.change_id(), &self.operator_id(), &self.original_capacity(), &self.original_expires_at(), self.code(), &&self.hash()[..], &self.key_id(), &self.created_at(), &self.operation_id(), &self.capacity(), &self.expires_at(), &self.used(), &self.withdrawn_at(), &self.changed_at(), &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version()],
                )
                .await?)
        }
    }
}

pub(in crate::access) async fn find(
    client: &Client,
    roles: &role::Reader,
    id: Id,
    read: history::Read,
    now: DateTime<Utc>,
) -> Result<Option<Verified<State>>, Error> {
    history::find(client, roles.keys(), roles.anchors(), read, id, now).await
}

pub(in crate::access) async fn for_role(
    client: &Client,
    roles: &role::Reader,
    role_id: role::Id,
    read: history::Read,
    now: DateTime<Utc>,
) -> Result<Vec<Verified<State>>, Error> {
    let rows = client
        .query(
            &format!("{} WHERE state.role_id=$1 ORDER BY state.id", State::SELECT),
            &[&role_id],
        )
        .await?;
    history::related(
        &rows,
        &Locator::new::<State>("role", role_id.as_bytes()),
        roles.keys(),
        roles.anchors(),
        read,
        now,
    )
}

/// Used by read paths that must not decode a row outside the creator's qualified locator.
pub(in crate::access) async fn locate_creator(
    client: &Client,
    roles: &role::Reader,
    id: Id,
    person_id: person::Id,
    read: history::Read,
    now: DateTime<Utc>,
) -> Result<Option<Verified<State>>, Error> {
    let row = client
        .query_opt(
            &format!("{} WHERE state.id=$1 AND state.person_id=$2", State::SELECT),
            &[&id, &person_id],
        )
        .await?;
    history::located(
        row.as_ref(),
        &Locator::new::<State>(
            "history",
            &[person_id.as_bytes().as_slice(), id.as_bytes().as_slice()].concat(),
        ),
        roles.keys(),
        roles.anchors(),
        read,
        now,
    )
}

pub(in crate::access) async fn locate_operation(
    client: &Client,
    roles: &role::Reader,
    operation_id: operation::Id,
    read: history::Read,
    now: DateTime<Utc>,
) -> Result<Option<Verified<State>>, Error> {
    let row = client
        .query_opt(
            &format!("{} WHERE state.operation_id=$1", State::SELECT),
            &[&operation_id],
        )
        .await?;
    history::located(
        row.as_ref(),
        &Locator::new::<State>("operation", operation_id.as_bytes()),
        roles.keys(),
        roles.anchors(),
        read,
        now,
    )
}

pub(in crate::access) async fn locate_code(
    client: &Client,
    keys: &super::keys::Keys,
    code: &super::Code,
    read: history::Read,
    roles: &role::Reader,
    now: DateTime<Utc>,
) -> Result<Option<Verified<State>>, grant::Error> {
    let mut ids = Vec::new();
    let mut hashes = Vec::new();
    for key in keys.lookup.all() {
        ids.push(*key.id().as_ref());
        hashes.push(code.lookup(key)?.to_vec());
    }
    let rows = client
        .query(
            &format!(
                "{} JOIN unnest($1::bigint[], $2::bytea[]) AS candidate (key_id, hash) ON candidate.key_id=state.key_id AND candidate.hash=state.hash",
                State::SELECT
            ),
            &[&ids, &hashes],
        )
        .await?;
    if rows.len() > 1 {
        return Err(
            Error::Integrity("an invitation code locates at most one retained digest").into(),
        );
    }
    let candidate = rows
        .first()
        .map(|row| {
            Ok::<_, Error>((
                row.try_get::<_, i64>("key_id").map_err(Error::decode)?,
                row.try_get::<_, Vec<u8>>("hash").map_err(Error::decode)?,
                row,
            ))
        })
        .transpose()?;
    let mut found = None;
    for (key_id, hash) in ids.iter().zip(&hashes) {
        let row = candidate
            .as_ref()
            .filter(|(id, digest, _)| id == key_id && digest == hash)
            .map(|(_, _, row)| *row);
        let mut locator = Vec::with_capacity(40);
        locator.extend_from_slice(&key_id.to_be_bytes());
        locator.extend_from_slice(hash);
        if let Some(state) = history::located(
            row,
            &Locator::new::<State>("code", &locator),
            roles.keys(),
            roles.anchors(),
            read,
            now,
        )? {
            found = Some(state);
        }
    }
    Ok(found)
}
