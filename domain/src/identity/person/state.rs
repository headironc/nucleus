//! The sealed permanent person state: object identity, current real-name revision and the
//! last transition. Its records live in [`super::record`].

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use zeroize::Zeroizing;

use crate::{
    history::{self, Anchors, Error, Keys, Kind, Locator, Read, Token, Verified},
    identity::person::{self, record::Record, revision},
    keys::seal::Seal,
};

pub(crate) const TABLE: &str = "identity.persons";

/// The Person lock belongs to the person object even while another owner discovers it.
pub(crate) async fn lock(
    client: &impl tokio_postgres::GenericClient,
    id: person::Id,
) -> Result<(), Error> {
    client
        .query_opt(
            "SELECT id FROM identity.persons WHERE id=$1 FOR UPDATE",
            &[&id],
        )
        .await?;
    Ok(())
}

/// Acquire the actor and target persons in their established global order.
pub(crate) async fn lock_participants(
    client: &Client,
    actor: Option<person::Id>,
    persons: &[person::Id],
) -> Result<(), Error> {
    client
        .query(
            "SELECT id FROM identity.persons WHERE id=$1 OR id=ANY($2) ORDER BY id FOR UPDATE",
            &[&actor, &persons],
        )
        .await?;
    Ok(())
}

/// A location hint for a not-yet-locked person. Authority and identity are loaded later.
pub(crate) async fn exists(
    client: &impl tokio_postgres::GenericClient,
    id: person::Id,
) -> Result<bool, crate::database::Query> {
    Ok(client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM identity.persons WHERE id=$1)",
            &[&id],
        )
        .await?
        .try_get(0)?)
}

#[derive(Clone)]
pub(crate) struct State {
    data: Data,
    sequence: i64,
    version: i64,
}

#[derive(Clone)]
pub(crate) struct Data {
    pub(super) id: person::Id,
    pub(super) revision_id: revision::Id,
    pub(super) changed_at: DateTime<Utc>,
}

impl State {
    pub(crate) fn id(&self) -> person::Id {
        self.data.id
    }

    pub(crate) fn revision_id(&self) -> revision::Id {
        self.data.revision_id
    }

    pub(crate) fn changed_at(&self) -> DateTime<Utc> {
        self.data.changed_at
    }
}

impl Kind for State {
    type Error = Error;
    const NAME: &'static str = "identity.person";
    const SELECT: &'static str = "SELECT state.id, state.revision_id, state.changed_at, state.sequence, state.version, state.seal, state.seal_key_id, state.seal_version FROM identity.persons state";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let changed_at = row.try_get("changed_at").map_err(Error::decode)?;
        if changed_at > now {
            return Err(Error::Integrity(
                "the person's last transition is not in the future",
            ));
        }
        Ok(Self {
            data: Data {
                id: row.try_get("id").map_err(Error::decode)?,
                revision_id: row.try_get("revision_id").map_err(Error::decode)?,
                changed_at,
            },
            sequence: row.try_get("sequence").map_err(Error::decode)?,
            version: row.try_get("version").map_err(Error::decode)?,
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        Zeroizing::new(vec![
            self.data.id.as_bytes().to_vec(),
            self.data.revision_id.as_bytes().to_vec(),
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
        self.data.id
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
        record.matches(self)
    }

    fn locators(&self) -> Vec<Locator> {
        Vec::new()
    }

    async fn write(
        &self,
        _: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> Result<u64, Error> {
        if let Some(original) = original {
            Ok(client.execute(
                "UPDATE identity.persons SET revision_id=$2, changed_at=$3, sequence=$4, version=$5, seal=$6, seal_key_id=$7, seal_version=$8 WHERE id=$1 AND version=$9 AND seal=$10 AND seal_key_id=$11 AND seal_version=$12",
                &[&self.data.id, &self.data.revision_id, &self.data.changed_at, &self.sequence,
                  &self.version, &seal.tag(), &seal.key_id(), &seal.version(),
                  &original.version, &original.seal().tag(), &original.seal().key_id(),
                  &original.seal().version()],
            ).await?)
        } else {
            Ok(client.execute(
                "INSERT INTO identity.persons (id, revision_id, changed_at, sequence, version, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
                &[&self.data.id, &self.data.revision_id, &self.data.changed_at, &self.sequence,
                  &self.version, &seal.tag(), &seal.key_id(), &seal.version()],
            ).await?)
        }
    }
}

pub(crate) async fn find(
    client: &Client,
    keys: Keys<'_>,
    anchors: &Anchors,
    read: Read,
    id: person::Id,
    now: DateTime<Utc>,
) -> Result<Option<Verified<State>>, Error> {
    history::find::<State>(client, keys, anchors, read, id, now).await
}

#[cfg(any(test, feature = "test-support"))]
pub(super) async fn age(
    client: &Client,
    keys: Keys<'_>,
    state: &Verified<State>,
    elapsed: chrono::TimeDelta,
) -> Result<Verified<State>, Error> {
    let mut data = state.data.clone();
    data.changed_at -= elapsed;
    let dated = history::Seed::state::<State>(data, state.sequence, state.version, keys)?;
    client.execute(
        "UPDATE identity.persons SET changed_at=$2,seal=$3,seal_key_id=$4,seal_version=$5 WHERE id=$1",
        &[&dated.id(), &dated.changed_at(), &dated.seal().tag(), &dated.seal().key_id(), &dated.seal().version()],
    ).await?;
    Ok(dated)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{Data, State};
    use crate::{
        history::{Kind as _, Seed},
        identity::{keys::encryption, person::revision},
        keys::{Derived, Entry, Id, Material, seal},
    };

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

    #[test]
    fn person_encoding_matches_an_independent_vector() {
        let encryption = encryption::Keys::new(derived());
        let seals = seal::Keys::new(derived());
        let keys = crate::history::Keys {
            data: &encryption,
            seals: &seals,
        };
        let state = Seed::state::<State>(
            Data {
                id: "00000000-0000-4000-8000-000000000001"
                    .parse()
                    .expect("person"),
                revision_id: "00000000-0000-4000-8000-000000000002"
                    .parse::<revision::Id>()
                    .expect("revision"),
                changed_at: chrono::DateTime::from_timestamp_micros(1_700_000_000_123_456)
                    .expect("time"),
            },
            3,
            7,
            keys,
        )
        .expect("sealed person fixture");
        let fields = state.fields();
        let encoded = seal::encode_fields(
            State::NAME,
            State::VERSION,
            &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )
        .expect("frozen person encoding");
        // Python struct.pack('>I'), raw UUID bytes, hashlib and hmac over the seal prefix.
        assert_eq!(encoded.as_ref().len(), 119);
        assert_eq!(
            hex::encode(platform::cryptography::sha256::Digest::new(&encoded).as_ref()),
            "a3fcb79ab6d4f7091c9fb80d3e9d7d8c016cdbaedfaefdda7411d9487edd6d27"
        );
        assert_eq!(
            hex::encode(state.seal().tag()),
            "20d51a8ae8eb426abb95f0f833fc3f2ca21382b1506f1a87cd8e0b93c4ccd019"
        );
        for index in 0..5 {
            let mut changed = state.fields();
            changed[index][0] ^= 1;
            assert!(
                seals
                    .check(
                        State::NAME,
                        State::VERSION,
                        &changed.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                        state.seal()
                    )
                    .is_err(),
                "field {index}"
            );
        }
    }
}
