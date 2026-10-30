//! The sealed current affiliation pointer; decisions never aggregate affiliation history.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use zeroize::Zeroizing;

use crate::{
    center::affiliation::{Id, database::Record},
    history::{Error, Keys, Kind, State, Token, Verified},
    identity::person,
    keys::seal::Seal,
};

pub(crate) const TABLE: &str = "center.profiles";

#[derive(Clone)]
pub(super) struct Profile {
    id: person::Id,
    affiliation_id: Id,
    created_at: DateTime<Utc>,
    sequence: i64,
    version: i64,
}

pub(super) struct Data {
    pub(super) person_id: person::Id,
    pub(super) affiliation_id: Id,
    pub(super) created_at: DateTime<Utc>,
}

impl Profile {
    pub(super) fn affiliation_id(&self) -> Id {
        self.affiliation_id
    }
    pub(super) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}

impl Kind for Profile {
    type Error = Error;
    const NAME: &'static str = "center.profile";
    const SELECT: &'static str = "SELECT state.id, state.affiliation_id, state.created_at, state.sequence, state.version, state.seal, state.seal_key_id, state.seal_version FROM center.profiles state";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let value = Self {
            id: row.try_get("id").map_err(Error::decode)?,
            affiliation_id: row.try_get("affiliation_id").map_err(Error::decode)?,
            created_at: row.try_get("created_at").map_err(Error::decode)?,
            sequence: row.try_get("sequence").map_err(Error::decode)?,
            version: row.try_get("version").map_err(Error::decode)?,
        };
        if value.created_at > now {
            return Err(Error::Integrity("a profile retains a non-future head"));
        }
        Ok(value)
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        Zeroizing::new(vec![
            self.id.as_bytes().to_vec(),
            self.affiliation_id.as_bytes().to_vec(),
            self.created_at.timestamp_micros().to_string().into_bytes(),
            self.sequence.to_string().into_bytes(),
            self.version.to_string().into_bytes(),
        ])
    }
}

pub(super) async fn verify(
    client: &Client,
    keys: Keys<'_>,
    anchors: &crate::history::Anchors,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let states = crate::history::inspect::<Profile>(client, keys, now).await?;
    for state in &states {
        if !person::state::exists(client, state.id).await? {
            return Err(Error::Integrity("a profile retains its permanent person"));
        }
    }
    crate::history::initialize(anchors, states)
}

impl State for Profile {
    type Id = person::Id;
    type Data = Data;
    type Record = Record;

    fn build(_: Token, data: Data, sequence: i64, version: i64) -> Self {
        Self {
            id: data.person_id,
            affiliation_id: data.affiliation_id,
            created_at: data.created_at,
            sequence,
            version,
        }
    }
    fn id(&self) -> person::Id {
        self.id
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
        self.affiliation_id == record.id() && self.created_at == record.created_at()
    }

    async fn write(
        &self,
        _: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> Result<u64, Error> {
        Ok(match original {
            None => client.execute("INSERT INTO center.profiles (id, affiliation_id, created_at, sequence, version, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)", &[&self.id, &self.affiliation_id, &self.created_at, &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version()]).await?,
            Some(original) => client.execute("UPDATE center.profiles SET affiliation_id=$2, created_at=$3, sequence=$4, version=$5, seal=$6, seal_key_id=$7, seal_version=$8 WHERE id=$1 AND version=$9 AND seal=$10 AND seal_key_id=$11 AND seal_version=$12", &[&self.id, &self.affiliation_id, &self.created_at, &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version(), &original.version(), &original.seal().tag(), &original.seal().key_id(), &original.seal().version()]).await?,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use chrono::DateTime;
    use platform::cryptography::sha256;
    use zeroize::Zeroizing;

    use super::{Data, Profile};
    use crate::{
        center::affiliation::Id,
        history::{self, Kind, Seed},
        identity::authentication,
        keys::{self, Derived, Entry, Material, seal},
    };

    #[test]
    fn profile_encoding_matches_the_independent_python_vector() {
        let key_id = keys::Id::new(0).expect("key");
        let seals = seal::Keys::new(Derived::fixed(
            key_id,
            vec![Entry::new(
                key_id,
                Material(Zeroizing::new(std::array::from_fn(|index| {
                    u8::try_from(index).expect("32 bytes")
                }))),
            )],
        ));
        let inventory = authentication::inventory(authentication::keys());
        let state = Seed::state::<Profile>(
            Data {
                person_id: serde_json::from_str("\"00000000-0000-4000-8000-000000000001\"")
                    .expect("person"),
                affiliation_id: Id(
                    uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000002").expect("record"),
                ),
                created_at: DateTime::from_timestamp_micros(1_700_000_000_123_000).expect("time"),
            },
            2,
            2,
            history::Keys {
                data: &inventory.encryption,
                seals: &seals,
            },
        )
        .expect("seed state");
        let fields = state.fields();
        let encoded = seal::encode_fields(
            Profile::NAME,
            Profile::VERSION,
            &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )
        .expect("encoding");
        // Python struct.pack(">I"), hashlib.sha256 and hmac with the independent 0..31 key.
        assert_eq!(encoded.as_ref().len(), 112);
        assert_eq!(
            hex::encode(sha256::Digest::new(&encoded).as_ref()),
            "777f6218a048731192c28c5ad4861fab1a5679c0985b43c319378551b7e2c453"
        );
        assert_eq!(
            hex::encode(state.seal().tag()),
            "1a0aa1d31b0a0a61e4c1020bd8d6b8a9a969d7ccd609775caf51544e3e87d37a"
        );
    }
}
