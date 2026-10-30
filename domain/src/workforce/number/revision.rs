//! Immutable number values. Original results load these fixed values independently of current state.

use chrono::{DateTime, Utc};
use tokio_postgres::{GenericClient, Row};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    history::{self, Error, Keys, Kind, Token},
    identity::person,
    keys::seal::Seal,
    workforce::number::{Id, Revision, state},
};

pub(crate) const TABLE: &str = "workforce.revisions";

#[derive(Clone, Copy)]
pub(super) enum Creation {
    Allocation(Uuid),
    Correction(Uuid),
    Recovery(Uuid),
}

impl Creation {
    fn columns(self) -> (Option<Uuid>, Option<Uuid>, Option<Uuid>) {
        match self {
            Self::Allocation(id) => (Some(id), None, None),
            Self::Correction(id) => (None, Some(id), None),
            Self::Recovery(id) => (None, None, Some(id)),
        }
    }
}

pub(super) struct Data {
    pub(super) person_id: person::Id,
    pub(super) revision: Revision,
    pub(super) creation: Creation,
}

pub(super) struct Value {
    data: Data,
}

impl Value {
    pub(super) fn person_id(&self) -> person::Id {
        self.data.person_id
    }
    pub(super) fn revision(&self) -> Revision {
        self.data.revision
    }
}

impl Kind for Value {
    type Error = Error;
    const NAME: &'static str = "workforce.revision";
    const SELECT: &'static str = "SELECT value.id,value.person_id,(value.number).subtype,(value.number).year,(value.number).serial,value.allocation_id,value.correction_id,value.recovery_id,value.seal,value.seal_key_id,value.seal_version FROM workforce.revisions value";
    fn decode(_: Token, row: &Row, _: Keys<'_>, _: DateTime<Utc>) -> Result<Self, Error> {
        let creation = match (
            row.try_get::<_, Option<Uuid>>("allocation_id")
                .map_err(Error::decode)?,
            row.try_get::<_, Option<Uuid>>("correction_id")
                .map_err(Error::decode)?,
            row.try_get::<_, Option<Uuid>>("recovery_id")
                .map_err(Error::decode)?,
        ) {
            (Some(id), None, None) => Creation::Allocation(id),
            (None, Some(id), None) => Creation::Correction(id),
            (None, None, Some(id)) => Creation::Recovery(id),
            _ => {
                return Err(Error::Integrity(
                    "a number version has exactly one creating record",
                ));
            }
        };
        Ok(Self {
            data: Data {
                person_id: row.try_get("person_id").map_err(Error::decode)?,
                revision: Revision {
                    id: row.try_get("id").map_err(Error::decode)?,
                    number: state::number(row)?,
                },
                creation,
            },
        })
    }
    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let revision = self.revision();
        let (allocation, correction, recovery) = self.data.creation.columns();
        Zeroizing::new(vec![
            revision.id.as_bytes().to_vec(),
            self.person_id().as_bytes().to_vec(),
            i16::from(revision.number.subtype())
                .to_string()
                .into_bytes(),
            revision.number.year().to_string().into_bytes(),
            revision.number.serial().to_string().into_bytes(),
            allocation
                .map(|id| id.as_bytes().to_vec())
                .unwrap_or_default(),
            correction
                .map(|id| id.as_bytes().to_vec())
                .unwrap_or_default(),
            recovery
                .map(|id| id.as_bytes().to_vec())
                .unwrap_or_default(),
        ])
    }
}

impl history::Value for Value {
    type Id = Id;
    type Data = Data;
    fn build(_: Token, data: Data) -> Self {
        Self { data }
    }
    fn id(&self) -> Id {
        self.revision().id
    }
    async fn insert(
        &self,
        _: Token,
        client: &(impl GenericClient + Sync),
        seal: &Seal,
    ) -> Result<(), Error> {
        let revision = self.revision();
        let number = revision.number;
        let (allocation, correction, recovery) = self.data.creation.columns();
        client.execute("INSERT INTO workforce.revisions (id,person_id,number,allocation_id,correction_id,recovery_id,seal,seal_key_id,seal_version) VALUES ($1,$2,ROW($3::smallint,$4::integer,$5::smallint)::workforce.number,$6,$7,$8,$9,$10,$11)",
            &[&revision.id,&self.person_id(),&i16::from(number.subtype()),&number.year(),&number.serial(),&allocation,&correction,&recovery,&seal.tag(),&seal.key_id(),&seal.version()]).await?;
        Ok(())
    }
}
