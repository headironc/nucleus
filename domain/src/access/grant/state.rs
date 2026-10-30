//! Current Workforce eligibility, unique occupancy and irreversible admission loss.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use zeroize::Zeroizing;

use crate::{
    access::{
        Entry, assignment,
        grant::{Id, Status, invitation, record::Record, suspension, termination::Mode},
        role,
    },
    history::{self, Error, Keys, Kind, Locator, Read, Token, Verified},
    identity::{
        account, operation,
        person::{self, revision},
    },
    keys::seal::Seal,
};

pub(crate) const TABLE: &str = "access.grants";

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use platform::cryptography::sha256;
    use serde::de::DeserializeOwned;

    use super::*;
    use crate::{
        access::grant::{
            record::{self, Event},
            transition,
        },
        history::{Seed, State as _},
        identity::authentication,
        keys::{self, Derived, Entry as KeyEntry, Material, seal},
    };

    fn id<T: DeserializeOwned>(value: u8) -> T {
        serde_json::from_value(serde_json::json!(format!(
            "00000000-0000-4000-8000-{value:012}"
        )))
        .expect("fixed ID")
    }

    fn cancellation_id(value: u8) -> crate::identity::cancellation::Id {
        use tokio_postgres::types::{FromSql, Type};
        let uuid: uuid::Uuid = id(value);
        crate::identity::cancellation::Id::from_sql(&Type::UUID, uuid.as_bytes())
            .expect("fixed cancellation ID")
    }

    fn data() -> Data {
        let now = DateTime::from_timestamp_micros(1_700_000_000_123_456).expect("time");
        Data {
            admission: Admission {
                id: id(1),
                person_id: id(2),
                operator_id: id(3),
                revision_id: id(4),
                entry: Some(Entry::Institution),
                invitation_id: None,
                granted_at: now,
                operation_id: id(5),
            },
            role_id: id(6),
            assignment_id: id(7),
            suspension_id: None,
            termination: None,
            epoch: 0,
            changed_at: now,
        }
    }

    fn check<K: Kind>(value: &Verified<K>, count: usize, length: usize, digest: &str, tag: &str) {
        let fields = value.fields();
        let encoded = seal::encode_fields(
            K::NAME,
            K::VERSION,
            &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )
        .expect("encoding");
        assert_eq!(fields.len(), count);
        assert_eq!(encoded.as_ref().len(), length);
        assert_eq!(hex::encode(sha256::Digest::new(&encoded).as_ref()), digest);
        assert_eq!(hex::encode(value.seal().tag()), tag);
    }

    #[test]
    fn encoding_matches_independent_python_vectors() {
        let key_id = keys::Id::new(0).expect("key ID");
        let seals = seal::Keys::new(Derived::fixed(
            key_id,
            vec![KeyEntry::new(
                key_id,
                Material(Zeroizing::new(std::array::from_fn(|index| {
                    u8::try_from(index).expect("32 bytes")
                }))),
            )],
        ));
        let inventory = authentication::inventory(authentication::keys());
        let keys = Keys {
            data: &inventory.encryption,
            seals: &seals,
        };
        // Independent Python UUID.bytes, struct.pack(">I"), SHA-256 and HMAC; key bytes 0..31.
        let mut data = data();
        check(
            &Seed::state::<State>(data.clone(), 1, 1, keys).expect("direct"),
            17,
            275,
            "ca25722b76af5b228ef1f224eb80854a7b32ba283ee56a0e911415f8829cade9",
            "c09aacc0632be87502182b7f1444c7cf5a87449df447bc82b0e7d696f2b9f93b",
        );
        data.admission.entry = None;
        data.admission.invitation_id = Some(id(8));
        data.suspension_id = Some(id(9));
        data.termination = Some(Mode::Cancellation);
        data.epoch = 2;
        check(
            &Seed::state::<State>(data, 7, 9, keys).expect("terminal invitation"),
            17,
            318,
            "859608933ea1e12e3b3a9d6219be4af0aed0e6363e757f7efa4d644dcd202ea3",
            "cd90b26f1b61098602938b79438ed1ad9faa7bf3a7a230cd2021f1ad098c143a",
        );
        let cases = [
            (
                1,
                None,
                None,
                0,
                Event::Assignment {
                    previous_id: None,
                    change_id: id(10),
                    entry: None,
                    operator_id: id(3),
                    revision_id: id(4),
                },
                16,
                267,
                "6ac71ac723a1a2be8b766100e7474596782edba3b39673b24658e07b15660ded",
                "fce9b66ceefafbe743ea1b95474e8a50d781b992ce37f327b394faa67e0bd5e1",
            ),
            (
                2,
                Some(id(9)),
                None,
                0,
                Event::Suspension {
                    id: id(9),
                    operator_id: id(3),
                    revision_id: id(4),
                },
                14,
                275,
                "dbef0529a1df99f0ae89d526d60bcc874ba42dec8d35d6b78a50c4420bea6c57",
                "bc62a8b33817e0b53810334c29444fe509289e0a13fbcf6c6893f14228047a4d",
            ),
            (
                3,
                None,
                None,
                0,
                Event::Resumption {
                    id: id(9),
                    operator_id: id(3),
                    revision_id: id(4),
                },
                14,
                259,
                "66f3d0440405d981b34df7e9e37d201ee1a096ce163102d95a2a88d9faa941e2",
                "83ce71843466f26256bc2a91f59dc2f44b395d854800f90c0d3e6fdd497e8906",
            ),
            (
                4,
                None,
                None,
                1,
                Event::Assignment {
                    previous_id: Some(id(11)),
                    change_id: id(10),
                    entry: Some(Entry::Domain),
                    operator_id: id(3),
                    revision_id: id(4),
                },
                16,
                289,
                "cb78f67d98815e52c42d761d89014b50689482e802d15fcdf07a8665fb0c8eb1",
                "5f2a39e259db040567f2f0d5f1ec8de173aa12cb630d4080949ab0bc31bde779",
            ),
            (
                5,
                None,
                None,
                2,
                Event::Loss {
                    id: id(12),
                    change_id: id(10),
                },
                13,
                233,
                "76047396c3fe1a96ae35bb174d60aed56cc00e02ebb0bfbf740228f8055cb1b4",
                "35e468285df4a9b928e0426c9e79805079ead7f2443b33aead11085485edec9b",
            ),
            (
                6,
                None,
                Some(Mode::Cancellation),
                2,
                Event::Termination {
                    mode: Mode::Cancellation,
                    operator_id: id(3),
                    revision_id: id(4),
                    cancellation_id: Some(cancellation_id(13)),
                },
                15,
                288,
                "d083d47b9954e3268113775aac5307cb3f5b76992fbd5aa6a9307ceefa188379",
                "dc2490828bc08e14467e0a763fbba1e002200c536b0df92baf8e10ee10078473",
            ),
        ];
        for (sequence, suspension_id, termination, epoch, event, count, length, digest, tag) in
            cases
        {
            let mut data = self::data();
            data.suspension_id = suspension_id;
            data.termination = termination;
            data.epoch = epoch;
            let record = Seed::record::<Record>(
                id(1),
                sequence,
                record::Data::new(&data, id(5), event),
                keys,
            )
            .expect("record");
            check(&record, count, length, digest, tag);
        }
    }

    #[test]
    fn lifecycle_preserves_epoch_and_rejects_stale_time_or_terminal_changes() {
        let inventory = authentication::inventory(authentication::keys());
        let keys = Keys {
            data: &inventory.encryption,
            seals: &inventory.seal,
        };
        let original = Seed::state::<State>(data(), 1, 1, keys).expect("active");
        let now = original.changed_at();
        assert!(
            transition::suspend(
                &original,
                id(9),
                id(3),
                id(4),
                id(5),
                now - chrono::Duration::microseconds(1)
            )
            .is_err()
        );
        let (suspended, record) = Seed::change(
            transition::suspend(&original, id(9), id(3), id(4), id(5), now)
                .expect("equal time is valid"),
            keys,
        )
        .expect("sealed suspension");
        assert_eq!(
            (suspended.sequence(), suspended.version(), suspended.epoch()),
            (2, 2, 0)
        );
        assert!(suspended.has_head(&record));
        assert!(transition::suspend(&suspended, id(10), id(3), id(4), id(5), now).is_err());
        assert!(transition::resume(&suspended, id(10), id(3), id(4), id(5), now).is_err());
        let (resumed, _) = Seed::change(
            transition::resume(&suspended, id(9), id(3), id(4), id(5), now)
                .expect("exact suspension"),
            keys,
        )
        .expect("resumed");
        assert_eq!(
            (resumed.sequence(), resumed.version(), resumed.epoch()),
            (3, 3, 0)
        );
        assert!(resumed.suspension_id().is_none());
        assert!(
            transition::terminate(&resumed, Mode::Cancellation, id(3), id(4), id(5), None, now)
                .is_err()
        );
        let (ended, _) = Seed::change(
            transition::terminate(
                &resumed,
                Mode::Cancellation,
                id(3),
                id(4),
                id(5),
                Some(cancellation_id(13)),
                now,
            )
            .expect("bound cancellation"),
            keys,
        )
        .expect("ended");
        assert_eq!(
            (ended.sequence(), ended.version(), ended.epoch()),
            (4, 4, 0)
        );
        assert!(!ended.unfinished());
        assert!(transition::suspend(&ended, id(9), id(3), id(4), id(5), now).is_err());
        assert!(
            transition::terminate(&ended, Mode::Manager, id(3), id(4), id(5), None, now).is_err()
        );
    }
}

/// The admission columns stay fixed when the object's current state advances.
#[derive(Clone)]
pub(super) struct Admission {
    pub(super) id: Id,
    pub(super) person_id: person::Id,
    pub(super) operator_id: revision::Id,
    pub(super) revision_id: revision::Id,
    pub(super) entry: Option<Entry>,
    pub(super) invitation_id: Option<invitation::Id>,
    pub(super) granted_at: DateTime<Utc>,
    pub(super) operation_id: operation::Id,
}

#[derive(Clone)]
pub(crate) struct State {
    data: Data,
    sequence: i64,
    version: i64,
}

#[derive(Clone)]
pub(crate) struct Data {
    pub(super) admission: Admission,
    pub(super) role_id: role::Id,
    pub(super) assignment_id: assignment::Id,
    pub(super) suspension_id: Option<suspension::Id>,
    pub(super) termination: Option<Mode>,
    pub(super) epoch: i64,
    pub(super) changed_at: DateTime<Utc>,
}

impl State {
    #[cfg(test)]
    pub(in crate::access) fn fixture_data(&self) -> Data {
        self.data.clone()
    }

    pub(crate) fn id(&self) -> Id {
        self.data.admission.id
    }
    pub(crate) fn person_id(&self) -> person::Id {
        self.data.admission.person_id
    }
    pub(in crate::access) fn operator_id(&self) -> revision::Id {
        self.data.admission.operator_id
    }
    pub(in crate::access) fn revision_id(&self) -> revision::Id {
        self.data.admission.revision_id
    }
    pub(in crate::access) fn entry(&self) -> Option<Entry> {
        self.data.admission.entry
    }
    pub(in crate::access) fn invitation_id(&self) -> Option<invitation::Id> {
        self.data.admission.invitation_id
    }
    pub(in crate::access) fn granted_at(&self) -> DateTime<Utc> {
        self.data.admission.granted_at
    }
    pub(in crate::access) fn operation_id(&self) -> operation::Id {
        self.data.admission.operation_id
    }
    pub(crate) fn role_id(&self) -> role::Id {
        self.data.role_id
    }
    pub(in crate::access) fn assignment_id(&self) -> assignment::Id {
        self.data.assignment_id
    }
    pub(in crate::access) fn suspension_id(&self) -> Option<suspension::Id> {
        self.data.suspension_id
    }
    pub(in crate::access) fn termination(&self) -> Option<Mode> {
        self.data.termination
    }
    pub(in crate::access) fn epoch(&self) -> i64 {
        self.data.epoch
    }
    pub(in crate::access) fn changed_at(&self) -> DateTime<Utc> {
        self.data.changed_at
    }
    pub(crate) fn unfinished(&self) -> bool {
        self.data.termination.is_none()
    }
    pub(crate) fn status(&self) -> Status {
        if self.data.termination.is_some() {
            Status::Terminated
        } else if self.data.suspension_id.is_some() {
            Status::Suspended
        } else {
            Status::Active
        }
    }

    pub(super) fn next(&self, now: DateTime<Utc>) -> Result<Data, Error> {
        if !self.unfinished() || now < self.changed_at() {
            return Err(Error::Integrity(
                "only an unfinished grant advances at a nondecreasing time",
            ));
        }
        let mut data = self.data.clone();
        data.changed_at = now;
        Ok(data)
    }
}

pub(super) fn termination_label(mode: Option<Mode>) -> Option<&'static str> {
    mode.map(|mode| match mode {
        Mode::Current => "self",
        Mode::Manager => "manager",
        Mode::Cancellation => "cancellation",
    })
}

pub(super) fn admission_label(entry: Option<Entry>) -> &'static str {
    match entry {
        Some(Entry::Domain) => "domain",
        Some(Entry::Institution) => "institution",
        None => "invitation",
    }
}

pub(super) fn parse_termination(value: Option<&str>) -> Result<Option<Mode>, Error> {
    Ok(match value {
        None => None,
        Some("self") => Some(Mode::Current),
        Some("manager") => Some(Mode::Manager),
        Some("cancellation") => Some(Mode::Cancellation),
        Some(_) => {
            return Err(Error::Integrity(
                "a grant retains its declared termination mode",
            ));
        }
    })
}

impl Kind for State {
    type Error = Error;
    const NAME: &'static str = "access.grant";
    const SELECT: &'static str = "SELECT state.id,state.person_id,state.domain,state.entry,state.invitation_id,state.operator_id,state.revision_id,state.granted_at,state.operation_id,state.role_id,state.assignment_id,state.suspension_id,state.termination,state.epoch,state.changed_at,state.sequence,state.version,state.seal,state.seal_key_id,state.seal_version FROM access.grants state";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let granted_at = row.try_get("granted_at").map_err(Error::decode)?;
        let changed_at = row.try_get("changed_at").map_err(Error::decode)?;
        let epoch = row.try_get("epoch").map_err(Error::decode)?;
        if row.try_get::<_, &str>("domain").map_err(Error::decode)? != "workforce"
            || granted_at > changed_at
            || changed_at > now
            || epoch < 0
        {
            return Err(Error::Integrity(
                "a Workforce grant retains non-future ordered times and a nonnegative epoch",
            ));
        }
        let entry = match row.try_get::<_, &str>("entry").map_err(Error::decode)? {
            "domain" => Some(Entry::Domain),
            "institution" => Some(Entry::Institution),
            "invitation" => None,
            _ => {
                return Err(Error::Integrity(
                    "a grant retains its declared admission source",
                ));
            }
        };
        let invitation_id: Option<invitation::Id> =
            row.try_get("invitation_id").map_err(Error::decode)?;
        if entry.is_none() != invitation_id.is_some() {
            return Err(Error::Integrity(
                "an invitation admission fixes exactly its source invitation",
            ));
        }
        Ok(Self {
            data: Data {
                admission: Admission {
                    id: row.try_get("id").map_err(Error::decode)?,
                    person_id: row.try_get("person_id").map_err(Error::decode)?,
                    operator_id: row.try_get("operator_id").map_err(Error::decode)?,
                    revision_id: row.try_get("revision_id").map_err(Error::decode)?,
                    entry,
                    invitation_id,
                    granted_at,
                    operation_id: row.try_get("operation_id").map_err(Error::decode)?,
                },
                role_id: row.try_get("role_id").map_err(Error::decode)?,
                assignment_id: row.try_get("assignment_id").map_err(Error::decode)?,
                suspension_id: row.try_get("suspension_id").map_err(Error::decode)?,
                termination: parse_termination(row.try_get("termination").map_err(Error::decode)?)?,
                epoch,
                changed_at,
            },
            sequence: row.try_get("sequence").map_err(Error::decode)?,
            version: row.try_get("version").map_err(Error::decode)?,
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        Zeroizing::new(vec![
            self.id().as_bytes().to_vec(),
            self.person_id().as_bytes().to_vec(),
            b"workforce".to_vec(),
            admission_label(self.entry()).as_bytes().to_vec(),
            self.invitation_id()
                .map(|id| id.as_bytes().to_vec())
                .unwrap_or_default(),
            self.operator_id().as_bytes().to_vec(),
            self.revision_id().as_bytes().to_vec(),
            self.granted_at()
                .timestamp_micros()
                .to_string()
                .into_bytes(),
            self.operation_id().as_bytes().to_vec(),
            self.role_id().as_bytes().to_vec(),
            self.assignment_id().as_bytes().to_vec(),
            self.suspension_id()
                .map(|id| id.as_bytes().to_vec())
                .unwrap_or_default(),
            termination_label(self.termination())
                .unwrap_or_default()
                .as_bytes()
                .to_vec(),
            self.epoch().to_string().into_bytes(),
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
    fn has_head(&self, record: &Record) -> bool {
        record.matches(self)
    }
    fn locators(&self) -> Vec<Locator> {
        let mut locations = vec![Locator::new::<Self>(
            "history",
            &[
                self.person_id().as_bytes().as_slice(),
                self.id().as_bytes().as_slice(),
            ]
            .concat(),
        )];
        if self.unfinished() {
            locations.push(Locator::new::<Self>(
                "workforce",
                self.person_id().as_bytes(),
            ));
            locations.push(Locator::new::<Self>(
                "role",
                &[
                    self.role_id().as_bytes().as_slice(),
                    self.id().as_bytes().as_slice(),
                ]
                .concat(),
            ));
        }
        locations
    }

    async fn write(
        &self,
        _: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> Result<u64, Error> {
        let termination = termination_label(self.termination());
        if let Some(original) = original {
            Ok(client.execute(
                "UPDATE access.grants SET role_id=$2,assignment_id=$3,suspension_id=$4,termination=$5,epoch=$6,changed_at=$7,sequence=$8,version=$9,seal=$10,seal_key_id=$11,seal_version=$12 WHERE id=$1 AND version=$13 AND seal=$14 AND seal_key_id=$15 AND seal_version=$16",
                &[&self.id(), &self.role_id(), &self.assignment_id(), &self.suspension_id(), &termination, &self.epoch(), &self.changed_at(), &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version(), &original.version, &original.seal().tag(), &original.seal().key_id(), &original.seal().version()],
            ).await?)
        } else {
            let entry = admission_label(self.entry());
            Ok(client.execute(
                "INSERT INTO access.grants (id,person_id,entry,operator_id,revision_id,granted_at,operation_id,role_id,assignment_id,suspension_id,termination,epoch,changed_at,sequence,version,seal,seal_key_id,seal_version,invitation_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)",
                &[&self.id(), &self.person_id(), &entry, &self.operator_id(), &self.revision_id(), &self.granted_at(), &self.operation_id(), &self.role_id(), &self.assignment_id(), &self.suspension_id(), &termination, &self.epoch(), &self.changed_at(), &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version(), &self.invitation_id()],
            ).await?)
        }
    }
}

pub(crate) async fn current(
    client: &Client,
    roles: &role::Reader,
    person_id: person::Id,
    read: Read,
    now: DateTime<Utc>,
) -> Result<Option<Verified<State>>, Error> {
    let row = client
        .query_opt(
            &format!(
                "{} WHERE state.person_id=$1 AND state.termination IS NULL",
                State::SELECT
            ),
            &[&person_id],
        )
        .await?;
    history::located(
        row.as_ref(),
        &Locator::new::<State>("workforce", person_id.as_bytes()),
        roles.keys(),
        roles.anchors(),
        read,
        now,
    )
}

#[cfg(test)]
pub(super) async fn retime(
    client: &Client,
    app: &crate::kernel::App,
    id: Id,
    admission_at: Option<DateTime<Utc>>,
    changed_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Verified<State>, Error> {
    use crate::history::{Seed, State as _};
    let original = history::find::<State>(client, app.keys(), app.anchors(), Read::Locked, id, now)
        .await?
        .ok_or(Error::Integrity("the ordering fixture retains its state"))?;
    let mut data = original.data.clone();
    if let Some(at) = admission_at {
        data.admission.granted_at = at;
    }
    data.changed_at = changed_at;
    let updated = Seed::state::<State>(data, original.sequence(), original.version(), app.keys())?;
    let seal = updated.seal();
    client.execute("UPDATE access.grants SET granted_at=$2,changed_at=$3,seal=$4,seal_key_id=$5,seal_version=$6 WHERE id=$1", &[&id,&updated.granted_at(),&changed_at,&seal.tag(),&seal.key_id(),&seal.version()]).await?;
    Ok(updated)
}

pub(in crate::access) async fn for_person(
    client: &Client,
    roles: &role::Reader,
    person_id: person::Id,
    read: Read,
    now: DateTime<Utc>,
) -> Result<Vec<Verified<State>>, Error> {
    let rows = client
        .query(
            &format!(
                "{} WHERE state.person_id=$1 ORDER BY state.granted_at DESC,state.id DESC",
                State::SELECT
            ),
            &[&person_id],
        )
        .await?;
    history::related(
        &rows,
        &Locator::new::<State>("history", person_id.as_bytes()),
        roles.keys(),
        roles.anchors(),
        read,
        now,
    )
}

pub(in crate::access) async fn find(
    client: &Client,
    roles: &role::Reader,
    id: Id,
    read: Read,
    now: DateTime<Utc>,
) -> Result<Option<Verified<State>>, Error> {
    history::find(client, roles.keys(), roles.anchors(), read, id, now).await
}

pub(crate) async fn lock_operator(client: &Client, account_id: account::Id) -> Result<(), Error> {
    let owner = account::state::discover_owner(client, account_id).await?;
    client.query("SELECT id FROM access.grants WHERE person_id=$1 AND termination IS NULL ORDER BY id FOR SHARE", &[&owner]).await?;
    Ok(())
}

pub(crate) async fn lock_person(client: &Client, person_id: person::Id) -> Result<(), Error> {
    client.query("SELECT id FROM access.grants WHERE person_id=$1 AND termination IS NULL ORDER BY id FOR UPDATE", &[&person_id]).await?;
    Ok(())
}

/// Cancellation recovery also authenticates previously ended memberships.
pub(crate) async fn lock_history(client: &Client, person_id: person::Id) -> Result<(), Error> {
    client
        .query(
            "SELECT id FROM access.grants WHERE person_id=$1 ORDER BY id FOR UPDATE",
            &[&person_id],
        )
        .await?;
    Ok(())
}

pub(in crate::access) async fn lock_shared(client: &Client, id: Id) -> Result<(), Error> {
    client
        .query_opt("SELECT id FROM access.grants WHERE id=$1 FOR SHARE", &[&id])
        .await?;
    Ok(())
}

pub(in crate::access) async fn lock_management(
    client: &Client,
    account_id: account::Id,
    target: Id,
) -> Result<(), Error> {
    let owner = account::state::discover_owner(client, account_id).await?;
    let rows = client.query("SELECT id FROM access.grants WHERE id=$1 OR (person_id=$2 AND termination IS NULL) ORDER BY id", &[&target, &owner]).await?;
    for row in rows {
        let id: Id = row.try_get("id").map_err(Error::decode)?;
        let sql = if id == target {
            "SELECT id FROM access.grants WHERE id=$1 FOR UPDATE"
        } else {
            "SELECT id FROM access.grants WHERE id=$1 FOR SHARE"
        };
        client.query_opt(sql, &[&id]).await?;
    }
    Ok(())
}

pub(crate) async fn lock_participants(
    client: &Client,
    account_id: account::Id,
    persons: &[person::Id],
) -> Result<(), Error> {
    let owner = account::state::discover_owner(client, account_id).await?;
    client.query("SELECT id FROM access.grants WHERE (person_id=$1 OR person_id=ANY($2)) AND termination IS NULL ORDER BY id FOR SHARE", &[&owner, &persons]).await?;
    Ok(())
}

/// Scope protection precedes discovery; the following load authenticates this complete set.
pub(in crate::access) async fn lock_role(
    client: &Client,
    account_id: account::Id,
    role_id: role::Id,
) -> Result<(), Error> {
    let owner = account::state::discover_owner(client, account_id).await?;
    let rows = client.query("SELECT id,role_id FROM access.grants WHERE termination IS NULL AND (role_id=$1 OR person_id=$2) ORDER BY id", &[&role_id, &owner]).await?;
    for row in rows {
        let id: Id = row.try_get("id").map_err(Error::decode)?;
        let held: role::Id = row.try_get("role_id").map_err(Error::decode)?;
        let sql = if held == role_id {
            "SELECT id FROM access.grants WHERE id=$1 FOR UPDATE"
        } else {
            "SELECT id FROM access.grants WHERE id=$1 FOR SHARE"
        };
        client.query_opt(sql, &[&id]).await?;
    }
    Ok(())
}

pub(in crate::access) async fn holders(
    client: &Client,
    roles: &role::Reader,
    role_id: role::Id,
    read: Read,
    now: DateTime<Utc>,
) -> Result<Vec<Verified<State>>, Error> {
    let rows = client
        .query(
            &format!(
                "{} WHERE state.role_id=$1 AND state.termination IS NULL ORDER BY state.id",
                State::SELECT
            ),
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
