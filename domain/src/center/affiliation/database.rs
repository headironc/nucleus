//! The immutable affiliation kind and its fixed SQL. Current decisions use Profile.

use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use zeroize::Zeroizing;

use crate::{
    center::affiliation::{Affiliation, Code, Error, Id, envelope},
    database::verification,
    history::{self, Kind, Token},
    identity::{account, current::Actor, operation, person, person::revision},
    keys::seal::Seal,
};

pub(crate) const TABLE: &str = "center.affiliations";

pub(super) struct Record {
    id: Id,
    person_id: person::Id,
    code: Code,
    operator_id: revision::Id,
    created_at: DateTime<Utc>,
    operation_id: operation::Id,
    sequence: i64,
}

pub(super) struct Data {
    pub(super) id: Id,
    pub(super) code: Code,
    pub(super) operator_id: revision::Id,
    #[cfg(test)]
    pub(super) operator_person: person::Id,
    pub(super) created_at: DateTime<Utc>,
    pub(super) operation_id: operation::Id,
}

impl Record {
    pub(super) fn id(&self) -> Id {
        self.id
    }
    pub(super) fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub(super) fn operator_id(&self) -> revision::Id {
        self.operator_id
    }
    pub(super) fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub(super) fn code(&self) -> Code {
        self.code
    }
    pub(super) fn affiliation(&self) -> Affiliation {
        Affiliation {
            value: self.code,
            created_at: self.created_at,
        }
    }
}

impl Kind for Record {
    type Error = history::Error;
    const NAME: &'static str = "center.affiliation";
    const SELECT: &'static str = "SELECT record.id, record.person_id, record.value, record.operator_id, record.created_at, record.operation_id, record.sequence, record.seal, record.seal_key_id, record.seal_version FROM center.affiliations record";

    fn decode(
        _: Token,
        row: &Row,
        keys: history::Keys<'_>,
        now: DateTime<Utc>,
    ) -> Result<Self, history::Error> {
        let id = row.try_get("id").map_err(history::Error::decode)?;
        let person_id = row.try_get("person_id").map_err(history::Error::decode)?;
        let created_at = row.try_get("created_at").map_err(history::Error::decode)?;
        let sequence = row.try_get("sequence").map_err(history::Error::decode)?;
        if created_at > now || sequence < 1 {
            return Err(history::Error::Integrity(
                "an affiliation retains a positive sequence and non-future time",
            ));
        }
        Ok(Self {
            id,
            person_id,
            created_at,
            sequence,
            code: envelope::open(
                keys.data,
                person_id,
                id,
                &row.try_get("value").map_err(history::Error::decode)?,
            )?,
            operator_id: row.try_get("operator_id").map_err(history::Error::decode)?,
            operation_id: row
                .try_get("operation_id")
                .map_err(history::Error::decode)?,
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        Zeroizing::new(vec![
            self.id.as_bytes().to_vec(),
            self.person_id.as_bytes().to_vec(),
            self.code.as_ref().as_bytes().to_vec(),
            self.operator_id.as_bytes().to_vec(),
            self.created_at.timestamp_micros().to_string().into_bytes(),
            self.operation_id.as_bytes().to_vec(),
            self.sequence.to_string().into_bytes(),
        ])
    }
}

impl history::Record for Record {
    const OBJECT: &'static str = "person_id";
    type Object = person::Id;
    type Data = Data;

    fn build(_: Token, person_id: person::Id, sequence: i64, data: Data) -> Self {
        Self {
            id: data.id,
            person_id,
            sequence,
            code: data.code,
            operator_id: data.operator_id,
            created_at: data.created_at,
            operation_id: data.operation_id,
        }
    }
    fn object(&self) -> person::Id {
        self.person_id
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
        keys: history::Keys<'_>,
        seal: &Seal,
    ) -> Result<(), history::Error> {
        let value = envelope::seal(keys.data, self.person_id, self.id, self.code)?;
        client.execute("INSERT INTO center.affiliations (id, person_id, value, operator_id, created_at, operation_id, sequence, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)", &[
            &self.id, &self.person_id, &value, &self.operator_id, &self.created_at,
            &self.operation_id, &self.sequence, &seal.tag(), &seal.key_id(), &seal.version(),
        ]).await?;
        Ok(())
    }
}

pub(super) async fn lock(client: &Client, actor: &Actor) -> Result<(), Error> {
    actor.lock_account(client).await?;
    account::state::lock_person(client, actor.account_id()).await?;
    Ok(())
}

pub(super) async fn lock_person<R>(
    client: &Client,
    person_id: person::Id,
) -> Result<(), crate::kernel::Error<R>> {
    crate::identity::person::state::lock(client, person_id).await?;
    Ok(())
}

pub(super) async fn person_exists(client: &Client, person_id: person::Id) -> Result<bool, Error> {
    Ok(crate::identity::person::state::exists(client, person_id).await?)
}

/// Startup confirms that every retained value keeps its original-operation completion.
pub(super) async fn operations(client: &Client) -> Result<Vec<operation::Id>, Error> {
    let mut result = Vec::new();
    let mut cursor: Option<operation::Id> = None;
    loop {
        let rows = verification::Stage::Affiliations
            .batch(
                Duration::from_secs(10),
                client.query(
                    &format!(
                        "SELECT operation_id FROM {TABLE} WHERE ($1::uuid IS NULL OR operation_id > $1) ORDER BY operation_id LIMIT 128"
                    ),
                    &[&cursor],
                ),
            )
            .await
            .map_err(history::Error::from)?
            .map_err(Error::query)?;
        if rows.is_empty() {
            return Ok(result);
        }
        for row in rows {
            let operation_id = row.try_get(0).map_err(Error::decode)?;
            cursor = Some(operation_id);
            result.push(operation_id);
        }
    }
}

pub(super) async fn lock_accounts(
    client: &Client,
    actor: account::Id,
    target: account::Id,
) -> Result<(), Error> {
    let mut ids = [actor, target];
    ids.sort_unstable_by_key(|id| *id.as_bytes());
    for id in ids {
        client
            .query_one("SELECT identity.lock_account($1)", &[&id])
            .await
            .map_err(Error::query)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use chrono::DateTime;
    use platform::cryptography::sha256;
    use zeroize::Zeroizing;

    use super::{Data, Record};
    use crate::{
        center::affiliation::{Code, Id},
        history::{self, Kind, Seed},
        identity::authentication,
        keys::{self, Derived, Entry, Material, seal},
    };

    #[test]
    fn affiliation_encoding_matches_the_independent_python_vector() {
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
        let record = Seed::record::<Record>(
            serde_json::from_str("\"00000000-0000-4000-8000-000000000001\"").expect("person"),
            2,
            Data {
                id: Id(
                    uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000002").expect("record"),
                ),
                code: Code::CommunistParty,
                operator_person: serde_json::from_str("\"00000000-0000-4000-8000-000000000003\"")
                    .expect("operator"),
                operator_id: serde_json::from_str("\"00000000-0000-4000-8000-000000000004\"")
                    .expect("revision"),
                created_at: DateTime::from_timestamp_micros(1_700_000_000_123_000).expect("time"),
                operation_id: "00000000-0000-4000-8000-000000000005"
                    .parse()
                    .expect("operation"),
            },
            history::Keys {
                data: &inventory.encryption,
                seals: &seals,
            },
        )
        .expect("seed record");
        let fields = record.fields();
        let encoded = seal::encode_fields(
            Record::NAME,
            Record::VERSION,
            &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )
        .expect("encoding");
        // Python struct.pack(">I"), hashlib.sha256 and hmac; the record sequence is signed.
        assert_eq!(encoded.as_ref().len(), 157);
        assert_eq!(
            hex::encode(sha256::Digest::new(&encoded).as_ref()),
            "9a4d8d9303143fc470684b27191d971ced9dfe5a0fd6875422bd2b47826e10f7"
        );
        assert_eq!(
            hex::encode(record.seal().tag()),
            "cb2a8c66573631c365e19e2ef06e7dfcb22675de7ff5d706c631c1ef2f9c9cf4"
        );
    }
}
