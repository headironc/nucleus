//! Complete correction history and its fixed operator identity for authorized management.

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    history::Read,
    identity::{
        correction::{Error, Id},
        person::{self, name::Name, number::IdNumber, revision},
    },
    pagination::Pagination,
};

pub struct List {
    count: i64,
    corrections: Vec<Correction>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn corrections(&self) -> &[Correction] {
        &self.corrections
    }
}

pub struct Correction {
    id: Id,
    before: Identity,
    after: Identity,
    operator: Operator,
    corrected_at: DateTime<Utc>,
}

impl Correction {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn before(&self) -> &Identity {
        &self.before
    }
    pub fn after(&self) -> &Identity {
        &self.after
    }
    pub fn operator(&self) -> &Operator {
        &self.operator
    }
    pub fn corrected_at(&self) -> DateTime<Utc> {
        self.corrected_at
    }
}

pub struct Identity {
    name: Name,
    id_number: IdNumber,
}

impl Identity {
    pub fn name(&self) -> &Name {
        &self.name
    }
    pub fn id_number(&self) -> &IdNumber {
        &self.id_number
    }

    fn from_profile(profile: revision::Profile) -> Self {
        let (_, name, id_number) = profile.into_values();
        Self { name, id_number }
    }
}

pub struct Operator {
    person_id: person::Id,
    name: Name,
}

impl Operator {
    pub fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub fn name(&self) -> &Name {
        &self.name
    }
}

pub(crate) async fn fetch_list(
    client: &Client,
    history: crate::history::Keys<'_>,
    anchors: &crate::history::Anchors,
    read: Read,
    person_id: person::Id,
    pagination: Pagination,
    now: DateTime<Utc>,
) -> Result<List, Error> {
    let (count, originals) =
        super::history_records(client, history, anchors, read, person_id, pagination, now).await?;
    let mut corrections = Vec::new();
    for original in originals {
        let person_id = original.operator.person_id();
        let (_, name, _) = original.operator.into_values();
        corrections.push(Correction {
            id: original.correction.id,
            before: Identity::from_profile(original.before),
            after: Identity::from_profile(original.after),
            operator: Operator { person_id, name },
            corrected_at: original.correction.corrected_at,
        });
    }
    Ok(List { count, corrections })
}
