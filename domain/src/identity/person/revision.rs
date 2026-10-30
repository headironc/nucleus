//! The immutable identity and independently bound fields of one real-name revision.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use platform::cryptography::envelope;
use serde::{Deserialize, Serialize};
use tokio_postgres::{
    GenericClient, Row,
    types::{FromSql, ToSql},
};
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    database::sealed::Sealed,
    history::{self, Kind, Token, Value as _, Verified},
    identity::{
        account, keys, person,
        person::{name::Name, number::IdNumber},
    },
    kernel::App,
    keys::seal::Seal,
};

pub(crate) const TABLE: &str = "identity.revisions";

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, FromSql, ToSql, ToSchema,
)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = person::revision::Id)]
pub struct Id(Uuid);

impl std::str::FromStr for Id {
    type Err = uuid::Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(input)?))
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Id {
    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

pub(crate) enum Field {
    Name,
    IdNumber,
}

struct Binding {
    id: Id,
    field: Field,
}

impl envelope::Binding for Binding {
    fn domain(&self) -> &[u8] {
        b"nucleus/identity/envelope/v1"
    }
    fn identity(&self) -> &[u8] {
        self.id.0.as_bytes()
    }
    fn field(&self) -> &[u8] {
        match self.field {
            Field::Name => b"revision.name",
            Field::IdNumber => b"revision.id_number",
        }
    }
}

pub(crate) struct Seals {
    pub(crate) name: Sealed,
    pub(crate) id_number: Sealed,
}

impl Seals {
    #[cfg(test)]
    pub(crate) fn seal(
        keys: &keys::encryption::Keys,
        id: Id,
        name: &Name,
        id_number: &IdNumber,
    ) -> Result<Self, Error> {
        Self::encrypt(keys, id, name, id_number)
    }

    fn encrypt(
        keys: &keys::encryption::Keys,
        id: Id,
        name: &Name,
        id_number: &IdNumber,
    ) -> Result<Self, Error> {
        struct Input<'a> {
            binding: Binding,
            value: &'a str,
        }
        impl envelope::Binding for Input<'_> {
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
        impl envelope::Input for Input<'_> {
            fn plaintext(&self) -> &[u8] {
                self.value.as_bytes()
            }
        }
        Ok(Self {
            name: Sealed(keys.seal(&Input {
                binding: Binding {
                    id,
                    field: Field::Name,
                },
                value: name.as_ref(),
            })?),
            id_number: Sealed(keys.seal(&Input {
                binding: Binding {
                    id,
                    field: Field::IdNumber,
                },
                value: id_number.as_ref(),
            })?),
        })
    }

    fn open(&self, keys: &keys::encryption::Keys, id: Id) -> Result<(Name, IdNumber), Error> {
        let name = keys.open(
            &self.name.0,
            &Binding {
                id,
                field: Field::Name,
            },
        )?;
        let id_number = keys.open(
            &self.id_number.0,
            &Binding {
                id,
                field: Field::IdNumber,
            },
        )?;
        let name_text = std::str::from_utf8(&name)?;
        let id_number_text = std::str::from_utf8(&id_number)?;
        let name: Name = name_text.parse()?;
        let id_number: IdNumber = id_number_text.parse()?;
        if name.as_ref() != name_text || id_number.as_ref() != id_number_text {
            return Err(Error::Canonical);
        }
        Ok((name, id_number))
    }
}

/// A prepared write retains the exact canonical values paired with its two envelopes.
#[derive(Clone)]
pub(crate) struct Prepared {
    revision_id: Id,
    person_id: person::Id,
    name: Name,
    id_number: IdNumber,
    seals: Arc<Seals>,
}

impl Prepared {
    pub(crate) fn new(
        keys: &keys::encryption::Keys,
        revision_id: Id,
        person_id: person::Id,
        name: &Name,
        id_number: &IdNumber,
    ) -> Result<Self, Error> {
        Ok(Self {
            revision_id,
            person_id,
            name: name.clone(),
            id_number: id_number.clone(),
            seals: Arc::new(Seals::encrypt(keys, revision_id, name, id_number)?),
        })
    }

    pub(crate) async fn write(
        self,
        client: &(impl GenericClient + Sync),
        keys: history::Keys<'_>,
    ) -> Result<(), Error> {
        Ok(history::insert_value::<Value>(client, keys, self).await?)
    }
}

#[derive(Clone)]
pub(crate) struct Value {
    data: Prepared,
}

impl Kind for Value {
    type Error = Error;
    const NAME: &'static str = "identity.revision";
    const SELECT: &'static str = "SELECT value.id,value.person_id,value.name,value.id_number,value.seal,value.seal_key_id,value.seal_version FROM identity.revisions value";

    fn decode(
        _: Token,
        row: &Row,
        keys: history::Keys<'_>,
        _: DateTime<Utc>,
    ) -> Result<Self, Error> {
        let revision_id = row.try_get("id").map_err(history::Error::decode)?;
        let seals = Seals {
            name: row.try_get("name").map_err(history::Error::decode)?,
            id_number: row.try_get("id_number").map_err(history::Error::decode)?,
        };
        let (name, id_number) = seals.open(keys.data, revision_id)?;
        Ok(Self {
            data: Prepared {
                revision_id,
                person_id: row.try_get("person_id").map_err(history::Error::decode)?,
                name,
                id_number,
                seals: Arc::new(seals),
            },
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        Zeroizing::new(vec![
            self.data.revision_id.as_bytes().to_vec(),
            self.data.person_id.as_bytes().to_vec(),
            self.data.name.as_ref().as_bytes().to_vec(),
            self.data.id_number.as_ref().as_bytes().to_vec(),
        ])
    }
}

impl history::Value for Value {
    type Id = Id;
    type Data = Prepared;

    fn build(_: Token, data: Prepared) -> Self {
        Self { data }
    }

    fn id(&self) -> Id {
        self.data.revision_id
    }

    async fn insert(
        &self,
        _: Token,
        client: &(impl GenericClient + Sync),
        seal: &Seal,
    ) -> Result<(), history::Error> {
        client.execute(
            "INSERT INTO identity.revisions (id,person_id,name,id_number,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7)",
            &[&self.data.revision_id,&self.data.person_id,&self.data.seals.name,&self.data.seals.id_number,&seal.tag(),&seal.key_id(),&seal.version()],
        ).await?;
        Ok(())
    }
}

/// Which permanent source a raw test fixture relies on.
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum FixtureSource {
    /// A synthetic retained version of an existing person shares its creation transaction.
    Person,
    /// Keep the enclosing transaction so a correction written beside it is the source.
    Transaction,
}

/// A raw historical fixture row with its independent field envelopes and history seal.
#[cfg(test)]
pub(crate) struct Fixture {
    revision_id: Id,
    person_id: person::Id,
    seals: Arc<Seals>,
    seal: Seal,
}

#[cfg(test)]
impl Fixture {
    pub(crate) async fn insert(
        self,
        client: &(impl GenericClient + Sync),
        source: FixtureSource,
    ) -> Result<(), tokio_postgres::Error> {
        let sql = match source {
            FixtureSource::Person => {
                "INSERT INTO identity.revisions (id,person_id,name,id_number,seal,seal_key_id,seal_version,transaction_id) SELECT $1,$2,$3,$4,$5,$6,$7,origin.transaction_id FROM identity.origins origin WHERE origin.person_id=$2"
            }
            FixtureSource::Transaction => {
                "INSERT INTO identity.revisions (id,person_id,name,id_number,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7)"
            }
        };
        client
            .execute(
                sql,
                &[
                    &self.revision_id,
                    &self.person_id,
                    &self.seals.name,
                    &self.seals.id_number,
                    &self.seal.tag(),
                    &self.seal.key_id(),
                    &self.seal.version(),
                ],
            )
            .await?;
        Ok(())
    }
}

/// Build a raw historical fixture row with freshly sealed canonical values.
#[cfg(test)]
pub(crate) fn fixture_builder(
    keys: history::Keys<'_>,
    revision_id: Id,
    person_id: person::Id,
    name: &Name,
    id_number: &IdNumber,
) -> Result<Fixture, Error> {
    let seals = Seals::encrypt(keys.data, revision_id, name, id_number)?;
    fixture_builder_sealed(keys, revision_id, person_id, name, id_number, seals)
}

#[cfg(test)]
pub(crate) fn fixture_builder_sealed(
    keys: history::Keys<'_>,
    revision_id: Id,
    person_id: person::Id,
    name: &Name,
    id_number: &IdNumber,
    seals: Seals,
) -> Result<Fixture, Error> {
    let prepared = Prepared {
        revision_id,
        person_id,
        name: name.clone(),
        id_number: id_number.clone(),
        seals: Arc::new(seals),
    };
    let seals = Arc::clone(&prepared.seals);
    let value = history::Seed::value::<Value>(prepared, keys)?;
    Ok(Fixture {
        revision_id,
        person_id,
        seals,
        seal: value.seal().clone(),
    })
}

/// Raw historical fixture insert with freshly sealed canonical values.
#[cfg(test)]
pub(crate) async fn fixture(
    client: &(impl GenericClient + Sync),
    keys: history::Keys<'_>,
    source: FixtureSource,
    revision_id: Id,
    person_id: person::Id,
    name: &Name,
    id_number: &IdNumber,
) -> Result<(), Error> {
    fixture_builder(keys, revision_id, person_id, name, id_number)?
        .insert(client, source)
        .await
        .map_err(history::Error::from)?;
    Ok(())
}

/// Raw historical fixture replacement with recomputed envelope and history seals.
#[cfg(test)]
pub(crate) async fn fixture_update(
    client: &(impl GenericClient + Sync),
    keys: history::Keys<'_>,
    revision_id: Id,
    person_id: person::Id,
    name: &Name,
    id_number: &IdNumber,
) -> Result<(), Error> {
    let prepared = Prepared::new(keys.data, revision_id, person_id, name, id_number)?;
    let value = history::Seed::value::<Value>(prepared.clone(), keys)?;
    let seal = value.seal();
    let updated = client
        .execute(
            "UPDATE identity.revisions SET name=$3,id_number=$4,seal=$5,seal_key_id=$6,seal_version=$7 WHERE id=$1 AND person_id=$2",
            &[
                &prepared.revision_id,
                &prepared.person_id,
                &prepared.seals.name,
                &prepared.seals.id_number,
                &seal.tag(),
                &seal.key_id(),
                &seal.version(),
            ],
        )
        .await.map_err(history::Error::from)?;
    if updated != 1 {
        return Err(Error::Missing);
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Profile {
    value: Verified<Value>,
}

impl Profile {
    pub(crate) fn person_id(&self) -> person::Id {
        self.value.data.person_id
    }
    pub(crate) fn revision_id(&self) -> Id {
        self.value.data.revision_id
    }
    pub(crate) fn name(&self) -> &Name {
        &self.value.data.name
    }
    pub(crate) fn id_number(&self) -> &IdNumber {
        &self.value.data.id_number
    }
    pub(crate) fn into_values(self) -> (Id, Name, IdNumber) {
        let data = self.value.into_inner().data;
        (data.revision_id, data.name, data.id_number)
    }
    pub(super) fn into_identity(self) -> Identity {
        Identity {
            person_id: self.person_id(),
            revision_id: self.revision_id(),
            name: self.name().masked(),
        }
    }
}

pub(crate) async fn fetch(
    client: &impl GenericClient,
    keys: history::Keys<'_>,
    id: Id,
) -> Result<Profile, Error> {
    // Permanent real-name values have no clock-dependent fields or acceptance rule.
    let value = history::find_value::<Value>(client, keys, id, DateTime::UNIX_EPOCH)
        .await?
        .ok_or(Error::Missing)?;
    Ok(Profile { value })
}

pub(crate) async fn fetch_many(
    client: &impl GenericClient,
    keys: history::Keys<'_>,
    ids: &[Id],
) -> Result<HashMap<Id, Profile>, Error> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let values = history::find_values::<Value>(client, keys, ids, DateTime::UNIX_EPOCH).await?;
    let profiles = values
        .into_iter()
        .map(|value| {
            let profile = Profile { value };
            (profile.revision_id(), profile)
        })
        .collect::<HashMap<_, _>>();
    if ids.iter().any(|id| !profiles.contains_key(id)) {
        return Err(Error::Missing);
    }
    Ok(profiles)
}

/// Source checks do not disclose a value or confer authenticated identity.
pub(crate) async fn verify_initial(
    client: &impl GenericClient,
    person_id: person::Id,
    id: Id,
) -> Result<(), history::Error> {
    super::record::verify_origin(client, person_id, id).await
}

/// Startup authenticates every value and its permanent origin or correction source.
pub(crate) async fn verify(
    client: &impl GenericClient,
    keys: history::Keys<'_>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let mut cursor: Option<Id> = None;
    loop {
        let rows = crate::database::verification::Stage::Revisions.query(
            std::time::Duration::from_secs(10), client
            .query(
                "SELECT id, person_id FROM identity.revisions WHERE ($1::uuid IS NULL OR id>$1) ORDER BY id LIMIT 128",
                &[&cursor],
            ))
            .await.map_err(history::Error::from)?
            .map_err(history::Error::from)?;
        if rows.is_empty() {
            return Ok(());
        }
        let mut pairs = Vec::with_capacity(rows.len());
        for row in &rows {
            let id = row.try_get("id").map_err(history::Error::decode)?;
            cursor = Some(id);
            pairs.push((
                id,
                row.try_get("person_id").map_err(history::Error::decode)?,
            ));
        }
        let ids = pairs.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        let values = crate::database::verification::Stage::Revisions
            .batch(
                std::time::Duration::from_secs(10),
                history::find_values::<Value>(client, keys, &ids, now),
            )
            .await
            .map_err(history::Error::from)??
            .into_iter()
            .map(|value| (value.id(), value))
            .collect::<HashMap<_, _>>();
        if values.len() != pairs.len() {
            return Err(Error::Missing);
        }
        let sourced = crate::database::verification::Stage::RevisionSources
            .batch(
                std::time::Duration::from_secs(10),
                super::record::sourced(client, &pairs),
            )
            .await
            .map_err(history::Error::from)??;
        for (id, person) in &pairs {
            let value = values.get(id).ok_or(Error::Missing)?;
            if value.data.person_id != *person || !sourced.contains(id) {
                return Err(Error::History(history::Error::Integrity(
                    "a real-name version retains its permanent source",
                )));
            }
        }
    }
}

/// A fixed identity, disclosed only after both encrypted fields validate.
#[derive(Clone)]
pub struct Identity {
    person_id: person::Id,
    revision_id: Id,
    name: String,
}

impl Identity {
    pub fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub fn revision_id(&self) -> Id {
        self.revision_id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl From<account::Subject> for Identity {
    fn from(subject: account::Subject) -> Self {
        Self {
            person_id: subject.person_id(),
            revision_id: subject.revision_id(),
            name: subject.name().to_owned(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Reader {
    app: App,
}

impl Reader {
    pub(crate) fn new(app: App) -> Self {
        Self { app }
    }

    pub(crate) async fn fetch_identity(
        &self,
        client: &tokio_postgres::Client,
        id: Id,
    ) -> Result<Identity, ReadError> {
        Ok(fetch(client, self.app.keys(), id)
            .await
            .map_err(ReadError::from)?
            .into_identity())
    }

    pub(crate) async fn fetch_identities(
        &self,
        client: &tokio_postgres::Client,
        ids: &[Id],
    ) -> Result<HashMap<Id, Identity>, ReadError> {
        Ok(fetch_many(client, self.app.keys(), ids)
            .await
            .map_err(ReadError::from)?
            .into_iter()
            .map(|(id, profile)| (id, profile.into_identity()))
            .collect())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("required fixed identity revision is missing")]
    Missing,
    #[error("fixed identity query failed")]
    Query(#[from] crate::database::Query),
    #[error("fixed identity decoding failed: {0}")]
    Decode(#[source] crate::database::Query),
    #[error("fixed identity revision is invalid")]
    Revision(#[source] Error),
}

impl ReadError {
    pub(crate) fn unreliable(&self) -> bool {
        match self {
            Self::Missing | Self::Decode(_) => true,
            Self::Revision(error) => error.unreliable(),
            Self::Query(_) => false,
        }
    }
}

impl From<Error> for ReadError {
    fn from(error: Error) -> Self {
        match error {
            Error::Missing => Self::Missing,
            Error::History(history::Error::Query(error)) => Self::Query(error),
            Error::History(history::Error::Decode(error)) => Self::Decode(error),
            other => Self::Revision(other),
        }
    }
}

impl From<tokio_postgres::Error> for ReadError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(error.into())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    History(#[from] history::Error),
    #[error("required fixed identity revision is missing")]
    Missing,
    #[error("the identity revision belongs to a different person")]
    Person,
    #[error("{0}")]
    Encryption(#[from] keys::encryption::Error),
    #[error("real-name plaintext is not UTF-8")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("real-name name is invalid")]
    Name(#[from] crate::identity::person::name::ParseError),
    #[error("real-name resident identity number is invalid")]
    Number(#[from] crate::identity::person::number::ParseError),
    #[error("real-name plaintext is not canonical")]
    Canonical,
}

impl Error {
    pub(crate) fn unreliable(&self) -> bool {
        match self {
            Self::History(error) => error.corrupt(),
            _ => true,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use platform::cryptography::envelope::Envelope;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use crate::{
        database::sealed::Sealed,
        history::{self, Kind},
        identity::{
            authentication::{Database, server},
            keys,
            person::revision::{Id, Prepared, Seals, Value},
        },
        keys::{Entry, Material},
    };

    #[test]
    fn seal_matches_independent_canonical_value_vector_and_ignores_reencryption() {
        let roots = crate::keys::tests::fixture();
        let seal_keys = crate::keys::seal::Keys::new(crate::keys::Derived::fixed(
            crate::keys::Id::new(0).expect("key id"),
            vec![Entry::new(
                crate::keys::Id::new(0).expect("key id"),
                Material(Zeroizing::new(std::array::from_fn(|index| {
                    u8::try_from(index).expect("32 bytes")
                }))),
            )],
        ));
        let keys = history::Keys {
            data: &roots.encryption,
            seals: &seal_keys,
        };
        let id = "00000000-0000-4000-8000-000000000003"
            .parse()
            .expect("revision");
        let person = "00000000-0000-4000-8000-000000000002"
            .parse()
            .expect("person");
        let name = "Fixture Operator".parse().expect("name");
        let number = "000000200002290010".parse().expect("number");
        let value = history::Seed::value::<Value>(
            Prepared::new(keys.data, id, person, &name, &number).expect("prepared"),
            keys,
        )
        .expect("value");
        let fields = value.fields();
        let fields = fields.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let encoded = crate::keys::seal::encode_fields(Value::NAME, 1, &fields).expect("encoded");
        // Python struct.pack('>I'), raw UUID bytes, canonical UTF-8, hashlib and hmac.
        assert_eq!(encoded.as_ref().len(), 127);
        assert_eq!(
            hex::encode(platform::cryptography::sha256::Digest::new(&encoded).as_ref()),
            "099a98f894859471c7145d1c8c737bc29e10a30e0c3fbb96aea246bd6285cdc9"
        );
        assert_eq!(
            hex::encode(value.seal().tag()),
            "284cecee74792d63a1844c8e433cd808f106d0854a2ad3ecdecd7b30bf39f650"
        );
        let reencrypted = history::Seed::value::<Value>(
            Prepared::new(keys.data, id, person, &name, &number).expect("fresh envelopes"),
            keys,
        )
        .expect("same value");
        assert_eq!(value.seal(), reencrypted.seal());
        for index in 0..4 {
            let mut changed = value.fields();
            changed[index][0] ^= 1;
            assert!(
                seal_keys
                    .check(
                        Value::NAME,
                        1,
                        &changed.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                        value.seal()
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn reads_reject_tampering_and_preserve_values_across_key_rotation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let roots = crate::keys::tests::fixture();
                let keys = history::Keys { data: &roots.encryption, seals: &roots.seal };
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                let client = owner.client();
                let id: Id = client.query_one("SELECT revision_id FROM identity.persons", &[]).await.expect("initial person").get(0);
                let original = super::fetch(client, keys, id).await.expect("sealed initial value");
                for sql in [
                    "UPDATE identity.revisions SET seal=set_byte(seal,0,get_byte(seal,0)#1)",
                    "ALTER TABLE identity.revisions DROP CONSTRAINT revisions_seal_version_check; UPDATE identity.revisions SET seal_version=2",
                    "UPDATE identity.revisions SET seal_key_id=999",
                    "UPDATE identity.revisions SET person_id=gen_random_uuid()",
                    "UPDATE identity.revisions SET id=gen_random_uuid()",
                ] {
                    client.batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated tampering");
                    client.batch_execute(sql).await.expect("fixture mutation");
                    assert!(super::fetch(client, keys, id).await.err().expect("no damaged value").unreliable(), "{sql}");
                    assert!(history::inspect_values::<Value>(client, keys, chrono::DateTime::UNIX_EPOCH).await.is_err(), "startup: {sql}");
                    client.batch_execute("ROLLBACK").await.expect("restore fixture");
                }
                client.batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE identity.revisions SET transaction_id=pg_current_xact_id()").await.expect("isolated source corruption");
                assert!(super::verify(client, keys, chrono::DateTime::UNIX_EPOCH).await.is_err());
                client.batch_execute("ROLLBACK").await.expect("restore source");
                client.batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("authentic plaintext alteration");
                let changed = Seals::encrypt(keys.data, id, &"Changed Operator".parse().expect("name"), original.id_number()).expect("authentic envelopes");
                client.execute("UPDATE identity.revisions SET name=$1 WHERE id=$2", &[&changed.name, &id]).await.expect("changed canonical name");
                assert!(matches!(super::fetch(client, keys, id).await, Err(super::Error::History(history::Error::Seal(_)))));
                client.batch_execute("ROLLBACK").await.expect("restore canonical identity");

                let rotated = crate::keys::Keys::new(crate::keys::Id::new(1).expect("active"),
                    vec![Entry::new(crate::keys::Id::new(0).expect("retained"), Material(Zeroizing::new([1;32]))),
                         Entry::new(crate::keys::Id::new(1).expect("active"), Material(Zeroizing::new([2;32])))],
                ).expect("rotated roots");
                let rotated_keys = history::Keys { data: &rotated.encryption, seals: &rotated.seal };
                super::fetch(client, rotated_keys, id).await.expect("retained encryption and seal keys");
                let before: Vec<u8> = client.query_one("SELECT seal FROM identity.revisions WHERE id=$1", &[&id]).await.expect("stored seal").get(0);
                let ciphertext = Seals::encrypt(rotated_keys.data, id, original.name(), original.id_number()).expect("new encryption key");
                client.batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("technical reencryption fixture");
                client.execute("UPDATE identity.revisions SET name=$1,id_number=$2 WHERE id=$3", &[&ciphertext.name, &ciphertext.id_number, &id]).await.expect("only envelopes change");
                let restored = super::fetch(client, rotated_keys, id).await.expect("old value seal authenticates reencrypted values");
                assert_eq!(restored.person_id(), original.person_id());
                assert_eq!(restored.name().as_ref(), original.name().as_ref());
                let after: Vec<u8> = client.query_one("SELECT seal FROM identity.revisions WHERE id=$1", &[&id]).await.expect("same seal").get(0);
                assert_eq!(before, after);
                client.batch_execute("ROLLBACK").await.expect("restore envelopes");

                assert!(matches!(super::fetch_many(client, keys, &[id, Id::allocate()]).await, Err(super::Error::Missing)));
                let ordinary = client.query_one("SELECT 1/0", &[]).await.expect_err("ordinary query failure");
                let error = crate::identity::account::Error::from(super::Error::History(history::Error::from(ordinary)));
                assert!(matches!(&error, crate::identity::account::Error::Database(_)));
                assert!(!crate::runtime::work::Failure::unreliable(&crate::registration::Error::Account(error)));
                owner.terminate().await.expect("owner ended");
            }).await.expect("bounded revision verification");
        });
    }

    #[test]
    fn opens_independent_field_vectors_with_their_retained_key_and_exact_revision() {
        let material = |id, text: &str| {
            Entry::new(
                crate::keys::Id::new(id).expect("key"),
                Material::new(Zeroizing::new(text.into())).expect("fixture material"),
            )
        };
        let keys = keys::encryption::Keys::new(crate::keys::Derived::fixed(
            crate::keys::Id::new(1).expect("active key"),
            vec![
                material(0, "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"),
                material(1, "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio"),
            ],
        ));
        // Python cryptography AESGCMSIV, u32be fields, key 00..1f, nonce 00..0b.
        let sealed = |text| {
            Sealed(Envelope::new(hex::decode(text).expect("fixed bytes")).expect("envelope"))
        };
        let seals = Seals {
            name: sealed(
                "010000000000000000000102030405060708090a0bf4727db6706d688a40deccc16298b31c801e765142fe1f20150eec6da4b86a72",
            ),
            id_number: sealed(
                "010000000000000000000102030405060708090a0b7fa5a373a7a56a098a6555484725c9c42e03c35c99ace908000ef9043b4b261b7def",
            ),
        };
        let id = Id(Uuid::parse_str("00000000-0000-4000-8000-000000000003").expect("revision"));
        let (name, id_number) = seals.open(&keys, id).expect("both exact original fields");
        assert_eq!(name.as_ref(), "Fixture Operator");
        assert_eq!(id_number.as_ref(), "000000200002290010");
        assert!(seals.open(&keys, Id::allocate()).is_err());
        let noncanonical = Seals {
            name: sealed(
                "010000000000000000000102030405060708090a0bee91377aefb7141faeafde2475ff49bcc7d360aecbb6d2969e7b5f2e6fca0106029d",
            ),
            id_number: sealed(
                "010000000000000000000102030405060708090a0b7fa5a373a7a56a098a6555484725c9c42e03c35c99ace908000ef9043b4b261b7def",
            ),
        };
        assert!(matches!(
            noncanonical.open(&keys, id),
            Err(super::Error::Canonical)
        ));
        let swapped = Seals {
            name: seals.id_number,
            id_number: seals.name,
        };
        assert!(swapped.open(&keys, id).is_err());
        let fresh = Seals::seal(&keys, id, &name, &id_number).expect("new independent envelopes");
        assert_ne!(
            &fresh.name.0.as_ref()[9..21],
            &fresh.id_number.0.as_ref()[9..21]
        );
        fresh.open(&keys, id).expect("complete roundtrip");
    }
}
