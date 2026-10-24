//! Self-visible correction history and shared authenticated pagination for management.

pub mod management;

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    history::{Read, State as _},
    identity::{
        correction::{Error, Id},
        person::{self, name::Name, record::Event, revision, state},
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
    pub fn corrected_at(&self) -> DateTime<Utc> {
        self.corrected_at
    }
}

pub struct Identity {
    name: Name,
    suffix: String,
}

impl Identity {
    pub fn name(&self) -> &Name {
        &self.name
    }
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    fn from_profile(profile: revision::Profile) -> Self {
        let (_, name, id_number) = profile.into_values();
        Self {
            name,
            suffix: id_number.suffix().to_owned(),
        }
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
        history_records(client, history, anchors, read, person_id, pagination, now).await?;
    let mut corrections = Vec::new();
    for original in originals {
        corrections.push(Correction {
            id: original.correction.id,
            before: Identity::from_profile(original.before),
            after: Identity::from_profile(original.after),
            corrected_at: original.correction.corrected_at,
        });
    }
    Ok(List { count, corrections })
}

/// Authenticate count and page in bounded batches, retaining only the requested page.
pub(super) async fn history_records(
    client: &Client,
    history: crate::history::Keys<'_>,
    anchors: &crate::history::Anchors,
    read: Read,
    person_id: person::Id,
    pagination: Pagination,
    now: DateTime<Utc>,
) -> Result<(i64, Vec<super::Original>), Error> {
    let person = state::find(client, history, anchors, read, person_id, now)
        .await?
        .ok_or(Error::Facts {
            rule: "correction history retains its permanent person",
        })?;
    let mut offset = 0;
    let mut count = 0_i64;
    let mut selected = Vec::new();
    while offset < person.sequence() {
        let size = (person.sequence() - offset).min(128);
        let records = crate::history::page(client, history, &person, size, offset, now).await?;
        let corrections = records
            .into_iter()
            .filter(|record| matches!(record.event(), Event::Correction { .. }))
            .collect::<Vec<_>>();
        // Empty and out-of-range requests retain the same source/completion validation.
        for original in super::restore(client, history, corrections, now).await? {
            if count >= pagination.offset() && count - pagination.offset() < pagination.size() {
                selected.push(original);
            }
            count = count.checked_add(1).ok_or(Error::Facts {
                rule: "the correction count is representable",
            })?;
        }
        offset += size;
    }
    Ok((count, selected))
}
