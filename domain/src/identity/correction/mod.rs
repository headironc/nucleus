//! Permanent real-name corrections and their immutable revision lineage.

pub mod admin;
pub mod history;

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    error,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::{
    Client, GenericClient,
    types::{FromSql, ToSql},
};
use utoipa::ToSchema;

use crate::{
    access::{appointment, holder, role},
    history::{Error as HistoryError, Keys as HistoryKeys, Record as _},
    identity::{
        account, keys,
        mobile::Mobile,
        operation,
        person::{
            self, name::Name, number::IdNumber, record, record::Event as RecordEvent, revision,
            state::State,
        },
    },
    kernel::{completion, operation::Family},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = correction::Id)]
pub struct Id(revision::Id);

impl Id {
    pub(crate) fn allocate() -> Self {
        Self(revision::Id::allocate())
    }
    pub(crate) fn from_revision(id: revision::Id) -> Self {
        Self(id)
    }
    pub(crate) fn revision_id(self) -> revision::Id {
        self.0
    }
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

/// The only executor family that turns an offline administrator correction into a permanent fact.
pub(crate) const COMPLETIONS: completion::Template =
    completion::Template::new(Family::SystemCorrection, |action, target| {
        action == "correct" && uuid::Uuid::parse_str(target).is_ok()
    });

pub(crate) const ONLINE: completion::Template =
    completion::Template::personal(Family::Correction, |action, target| {
        action == "correct"
            && target
                .parse::<person::Id>()
                .is_ok_and(|id| id.to_string() == target)
    });

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = correction::Correction)]
#[derive(Clone, Copy)]
pub struct Correction {
    id: Id,
    #[schema(value_type = String, format = DateTime)]
    corrected_at: DateTime<Utc>,
}

impl Correction {
    fn new(id: Id, corrected_at: DateTime<Utc>) -> Self {
        Self { id, corrected_at }
    }
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn corrected_at(&self) -> DateTime<Utc> {
        self.corrected_at
    }
}

/// The immutable appointment definition decides which path accepted a retained correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    Online,
    System,
}

/// The canonical input retained by a system correction completion. It never omits the
/// original operator mobile or any confirmed target value.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Canonical<'a> {
    pub(crate) operator: CanonicalOperator<'a>,
    pub(crate) target: CanonicalTarget,
    pub(crate) name: &'a Name,
    pub(crate) id_number: &'a IdNumber,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CanonicalOperator<'a> {
    pub(crate) mobile: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CanonicalTarget {
    pub(crate) person_id: person::Id,
    pub(crate) revision_id: revision::Id,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Confirmed {
    operator: ConfirmedOperator,
    target: ConfirmedTarget,
    name: Name,
    id_number: IdNumber,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmedOperator {
    mobile: Mobile,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfirmedTarget {
    person_id: person::Id,
    revision_id: revision::Id,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfirmedOnline {
    target: ConfirmedTarget,
    name: Name,
    id_number: IdNumber,
}

pub(crate) async fn person_exists(
    client: &impl GenericClient,
    id: person::Id,
) -> Result<bool, Error> {
    Ok(person::state::exists(client, id).await?)
}

pub(crate) async fn lock_person(client: &impl GenericClient, id: person::Id) -> Result<(), Error> {
    Ok(person::state::lock(client, id).await?)
}

pub(crate) struct Original {
    correction: Correction,
    operation_id: operation::Id,
    person_id: person::Id,
    operator: revision::Profile,
    before: revision::Profile,
    after: revision::Profile,
    source: Source,
}

impl Original {
    pub(crate) fn source(&self) -> Source {
        self.source
    }
    pub(crate) fn changes(&self) -> (bool, bool) {
        (
            self.before.name().as_ref() != self.after.name().as_ref(),
            self.before.id_number().as_ref() != self.after.id_number().as_ref(),
        )
    }
    pub(crate) fn matches(
        &self,
        person_id: person::Id,
        revision_id: revision::Id,
        operator_id: person::Id,
        name: &Name,
        id_number: &IdNumber,
    ) -> bool {
        self.person_id == person_id
            && self.before.revision_id() == revision_id
            && self.operator.person_id() == operator_id
            && self.after.name().as_ref() == name.as_ref()
            && self.after.id_number().as_ref() == id_number.as_ref()
    }
    /// A system replay also retains the original operator revision, not only its person.
    pub(crate) fn matches_system(
        &self,
        person_id: person::Id,
        revision_id: revision::Id,
        operator_person: person::Id,
        operator_revision: revision::Id,
        name: &Name,
        id_number: &IdNumber,
    ) -> bool {
        self.operator.revision_id() == operator_revision
            && self.matches(person_id, revision_id, operator_person, name, id_number)
    }
    pub(crate) fn into_correction(self) -> Correction {
        self.correction
    }
}

struct CorrectionRecord {
    id: Id,
    sequence: i64,
    operation_id: operation::Id,
    person_id: person::Id,
    operator_id: revision::Id,
    corrected_at: DateTime<Utc>,
    before_id: revision::Id,
    appointment_id: Option<appointment::Id>,
}

impl CorrectionRecord {
    fn from_record(record: &crate::history::Verified<record::Record>) -> Result<Self, Error> {
        let RecordEvent::Correction {
            id,
            revision_id,
            before_id,
            operator_id,
            appointment_id,
        } = record.event()
        else {
            return Err(Error::Facts {
                rule: "a correction loader selects correction records",
            });
        };
        if id.revision_id() != *revision_id {
            return Err(Error::Facts {
                rule: "a correction identity is its after-revision identity",
            });
        }
        Ok(Self {
            id: *id,
            sequence: record.sequence(),
            operation_id: record.operation_id(),
            person_id: record.person_id(),
            operator_id: *operator_id,
            corrected_at: record.happened_at(),
            before_id: *before_id,
            appointment_id: *appointment_id,
        })
    }
}

fn authenticate(
    record: &completion::Record,
    history: HistoryKeys<'_>,
    fact: &CorrectionRecord,
    operator: &revision::Profile,
    before: &revision::Profile,
    after: &revision::Profile,
) -> Result<(), Error> {
    authenticate_identity(record, fact, operator, true)?;
    let confirmed: Confirmed = record.input(history)?;
    if confirmed.target.person_id != fact.person_id
        || confirmed.target.revision_id != before.revision_id()
        || confirmed.name.as_ref() != after.name().as_ref()
        || confirmed.id_number.as_ref() != after.id_number().as_ref()
    {
        return Err(Error::Facts {
            rule: "system correction completion keeps its confirmed target values",
        });
    }
    // The sealed mobile preserves the original attribution, independent of today's account.
    let _ = &confirmed.operator.mobile;
    Ok(())
}

fn authenticate_identity(
    record: &completion::Record,
    fact: &CorrectionRecord,
    operator: &revision::Profile,
    system: bool,
) -> Result<(), Error> {
    if record.operation_id() != fact.operation_id
        || record.system() != system
        || record.action() != "correct"
        || record.target() != fact.person_id.to_string()
        || record.person_id() != operator.person_id()
        || record.revision_id() != fact.operator_id
        || record.created_at() != fact.corrected_at
    {
        return Err(Error::Facts {
            rule: "correction completion keeps its operation, operator and time",
        });
    }
    Ok(())
}

/// A page or startup batch loads each kind of fixed reference in one query.
async fn restore(
    client: &(impl GenericClient + Sync),
    history: HistoryKeys<'_>,
    records: Vec<crate::history::Verified<record::Record>>,
    now: DateTime<Utc>,
) -> Result<Vec<Original>, Error> {
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let facts = records
        .iter()
        .map(CorrectionRecord::from_record)
        .collect::<Result<Vec<_>, _>>()?;
    let operations = facts
        .iter()
        .map(|fact| fact.operation_id)
        .collect::<Vec<_>>();
    let mut completions = HashMap::new();
    for record in completion::retained(client, history, COMPLETIONS, &operations).await? {
        if completions.insert(record.operation_id(), record).is_some() {
            return Err(Error::Facts {
                rule: "a system correction retains one completion",
            });
        }
    }
    let mut online = HashMap::new();
    for record in completion::retained(client, history, ONLINE, &operations).await? {
        if online.insert(record.operation_id(), record).is_some() {
            return Err(Error::Facts {
                rule: "an online correction retains one completion",
            });
        }
    }
    let mut persons = facts.iter().map(|fact| fact.person_id).collect::<Vec<_>>();
    persons.sort_by_key(|person| *person.as_bytes());
    persons.dedup();
    let mut origins = HashMap::with_capacity(persons.len());
    for person in &persons {
        let origin = record::origin(client, history, *person, now)
            .await?
            .ok_or(Error::Facts {
                rule: "a corrected person retains its permanent origin",
            })?;
        origins.insert(*person, origin);
    }
    let ids = facts
        .iter()
        .filter_map(|fact| fact.appointment_id)
        .collect::<Vec<_>>();
    let revision_ids = facts
        .iter()
        .flat_map(|fact| {
            [
                Some(fact.id.revision_id()),
                Some(fact.operator_id),
                Some(fact.before_id),
            ]
        })
        .flatten()
        .collect::<Vec<_>>();
    let profiles = revision::fetch_many(client, history, &revision_ids).await?;
    let appointments = holder::appointments(client, history, &ids, now).await?;
    let selections = appointments
        .iter()
        .map(|record| {
            record
                .assignment()
                .map(|assignment| (record.role_id(), assignment.change_id))
                .ok_or(Error::Facts {
                    rule: "correction refers to an appointment",
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let definitions = role::definitions(client, history, &selections, now).await?;
    let projection_ids = facts.iter().map(|fact| fact.id).collect::<Vec<_>>();
    let projections =
        account::record::corrections(client.client(), history, &projection_ids, now).await?;
    facts
        .into_iter()
        .map(|fact| {
            let origin = origins
                .get(&fact.person_id)
                .ok_or(Error::Facts {
                    rule: "a corrected person retains its permanent origin",
                })?;
            let RecordEvent::Origin { .. } = origin.event()
            else {
                return Err(Error::Facts {
                    rule: "the first person record is its origin",
                });
            };
            if fact.sequence <= 1 || fact.corrected_at < origin.happened_at() {
                return Err(Error::Facts {
                    rule: "correction follows its permanent origin",
                });
            }
            if fact.sequence == 2 && fact.before_id != origin.event().revision_id() {
                return Err(Error::Facts {
                    rule: "the first correction follows the origin revision",
                });
            }
            let appointment = fact
                .appointment_id
                .map(|id| {
                    appointments
                        .iter()
                        .zip(&definitions)
                        .find(|(record, _)| {
                            record
                                .assignment()
                                .is_some_and(|assignment| assignment.id == id)
                        })
                        .ok_or(Error::Facts {
                            rule: "correction retains its fixed appointment and definition",
                        })
                })
                .transpose()?;
            let operation_id = fact.operation_id;
            let profile = |id| profiles.get(&id).cloned().ok_or(revision::Error::Missing);
            let operator = profile(fact.operator_id)?;
            let after = profile(fact.id.revision_id())?;
            let before = profile(fact.before_id)?;
            if after.person_id() != fact.person_id
                || before.person_id() != fact.person_id
                || before.revision_id() != fact.before_id
            {
                return Err(revision::Error::Person.into());
            }
            let administered = match appointment {
                Some((record, role)) => {
                    if record.person_id() != fact.person_id {
                        return Err(Error::Facts {
                            rule: "correction captures its own person's appointment",
                        });
                    }
                    role.kind() == role::Kind::Full
                }
                None => false,
            };
            if !administered && operator.person_id() == fact.person_id {
                return Err(Error::Facts {
                    rule: "online correction retains a non-self operator",
                });
            }
            if fact.corrected_at > now {
                return Err(Error::Facts {
                    rule: "correction precedes trusted time",
                });
            }
            if after.name().as_ref() == before.name().as_ref()
                && after.id_number().as_ref() == before.id_number().as_ref()
            {
                return Err(Error::Facts {
                    rule: "correction changes the original real-name definition",
                });
            }
            let source = if administered {
                Source::System
            } else {
                Source::Online
            };
            let completion = completions.get(&operation_id);
            let online = online.get(&operation_id);
            if source == Source::System {
                if online.is_some() {
                    return Err(Error::Facts {
                        rule: "a correction has exactly one completion source",
                    });
                }
                authenticate(
                    completion.ok_or(Error::Facts {
                        rule: "system correction retains its authenticated completion",
                    })?,
                    history,
                    &fact,
                    &operator,
                    &before,
                    &after,
                )?;
            } else if completion.is_some() {
                return Err(Error::Facts {
                    rule: "online correction retains no system completion",
                });
            } else {
                let record = online.ok_or(Error::Facts {
                    rule: "online correction retains its authenticated completion",
                })?;
                authenticate_identity(record, &fact, &operator, false)?;
                let confirmed: ConfirmedOnline = record.input(history)?;
                if confirmed.target.person_id != fact.person_id
                    || confirmed.target.revision_id != fact.before_id
                    || confirmed.name.as_ref() != after.name().as_ref()
                    || confirmed.id_number.as_ref() != after.id_number().as_ref()
                {
                    return Err(Error::Facts {
                        rule: "online correction completion keeps its confirmed target values",
                    });
                }
            }
            let original = Original {
                correction: Correction::new(fact.id, fact.corrected_at),
                operation_id: fact.operation_id,
                person_id: fact.person_id,
                operator,
                before,
                after,
                source,
            };
            let current_account = fact.appointment_id.is_some();
            let projection = projections.iter().find(|record|
                matches!(record.event(), account::record::Event::Correction(id) if *id == original.correction.id));
            if original.changes().1 && current_account {
                let projection = projection.ok_or(Error::Facts {
                    rule: "a changed current identity number retains its account projection",
                })?;
                let expected_account = appointment
                    .map(|(record, _)| record.holder_id())
                    .ok_or(Error::Facts {
                        rule: "a changed current identity number retains its appointment",
                    })?;
                if projection.account_id() != expected_account
                    || projection.person_id() != original.person_id
                    || projection.operation_id() != operation_id
                    || projection.happened_at() != original.correction.corrected_at
                {
                    return Err(Error::Facts { rule: "the account projection retains the original correction" });
                }
            } else if projection.is_some() {
                return Err(Error::Facts { rule: "only a changed current identity number projects onto its account" });
            }
            Ok(original)
        })
        .collect()
}

pub(crate) async fn find_by_operation(
    client: &(impl GenericClient + Sync),
    history: HistoryKeys<'_>,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<Original>, Error> {
    match record::find_correction(client, history, operation_id, now).await? {
        Some(record) => restore(client, history, vec![record], now)
            .await
            .map(|mut originals| originals.pop()),
        None => Ok(None),
    }
}

/// A miss in one completion family cannot hide a retained or orphaned other source.
pub(crate) async fn absent<R>(
    tx: &crate::kernel::tx::Tx,
    app: &crate::kernel::App,
    source: Source,
) -> Result<(), crate::kernel::Error<R>> {
    let now = tx.now(app.context()).await?;
    let record = record::find_correction(tx.client(), app.keys(), tx.operation_id(), now).await?;
    let Some(record) = record else {
        for template in [COMPLETIONS, ONLINE] {
            if completion::original::<R>(tx.client(), app.keys(), template, tx.operation_id())
                .await?
                .is_some()
            {
                return Err(crate::kernel::Error::corrupt(
                    "a retained correction completion keeps its permanent fact",
                ));
            }
        }
        return Ok(());
    };
    let original = restore(tx.client(), app.keys(), vec![record], now)
        .await?
        .pop()
        .ok_or_else(|| {
            crate::kernel::Error::corrupt("a retained correction restores its source")
        })?;
    if original.source != source {
        return Err(crate::kernel::Error::Mismatch);
    }
    Err(crate::kernel::Error::corrupt(
        "a retained correction keeps its completion",
    ))
}

pub(crate) struct New<'a> {
    pub(crate) id: Id,
    pub(crate) person_id: person::Id,
    pub(crate) operator_id: revision::Id,
    pub(crate) appointment_id: Option<appointment::Id>,
    pub(crate) name: &'a Name,
    pub(crate) id_number: &'a IdNumber,
    pub(crate) operation_id: operation::Id,
    pub(crate) corrected_at: DateTime<Utc>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the shared writer keeps verified Person, value and optional Account facts separate from its write context and pending owner"
)]
pub(crate) async fn write(
    client: &(impl GenericClient + Sync),
    history: HistoryKeys<'_>,
    anchors: &crate::history::Anchors,
    number: &keys::number::Keys,
    state: &crate::history::Verified<State>,
    previous: &revision::Profile,
    account: Option<&crate::history::Verified<account::state::State>>,
    pending: &mut Option<crate::history::Pending>,
    input: New<'_>,
) -> Result<Correction, Error> {
    if previous.person_id() != input.person_id
        || state.id() != input.person_id
        || previous.revision_id() != state.revision_id()
    {
        return Err(revision::Error::Person.into());
    }
    let projection = if previous.id_number().as_ref() != input.id_number.as_ref()
        && let Some(account) = account
        && account.current().is_some()
    {
        if account.person_id() != input.person_id {
            return Err(Error::Facts {
                rule: "a correction projects onto its own person's account",
            });
        }
        Some(account::transition::correct(
            account,
            input.id,
            number.hash(input.id_number)?,
            input.operation_id,
            input.corrected_at,
        )?)
    } else {
        None
    };
    let change = person::transition::correct(
        state,
        previous.revision_id(),
        input.id.revision_id(),
        input.operator_id,
        input.appointment_id,
        input.operation_id,
        input.corrected_at,
    )?;
    let revision = revision::Prepared::new(
        history.data,
        input.id.revision_id(),
        input.person_id,
        input.name,
        input.id_number,
    )?;
    crate::history::retain(
        pending,
        change.write(client.client(), history, anchors).await?,
    )?;
    revision.write(client, history).await?;
    if let Some(projection) = projection {
        crate::history::retain(
            pending,
            projection.write(client.client(), history, anchors).await?,
        )?;
    }
    Ok(Correction::new(input.id, input.corrected_at))
}

pub(crate) async fn verify(
    client: &Client,
    history: HistoryKeys<'_>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let mut operations = HashSet::new();
    let mut cursor = None;
    loop {
        let records = record::batch(
            client,
            history,
            "correction",
            crate::database::verification::Stage::Corrections,
            cursor,
            now,
        )
        .await?;
        let Some(last) = records.last() else {
            break;
        };
        cursor = Some((last.person_id(), last.sequence()));
        let originals = crate::database::verification::Stage::Corrections
            .batch(
                std::time::Duration::from_secs(10),
                restore(client, history, records, now),
            )
            .await??;
        for original in originals {
            if !operations.insert((original.source, original.operation_id)) {
                return Err(Error::Facts {
                    rule: "a correction retains one original operation",
                });
            }
        }
    }
    for (source, template) in [(Source::System, COMPLETIONS), (Source::Online, ONLINE)] {
        for record in completion::fetch_all(client, history, template).await? {
            if !operations.remove(&(source, record.operation_id())) {
                return Err(Error::Facts {
                    rule: "a retained completion belongs to one correction of its source",
                });
            }
        }
    }
    if !operations.is_empty() {
        return Err(Error::Facts {
            rule: "a retained correction keeps its completion",
        });
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    History(#[from] HistoryError),
    #[error("{0}")]
    Role(#[from] role::Error),
    #[error("persisted correction facts are inconsistent: {rule}")]
    Facts { rule: &'static str },
    #[error("correction query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("{0}")]
    Revision(#[from] revision::Error),
    #[error("correction identity-number digest computation failed")]
    Crypto(#[from] platform::cryptography::Error),
    #[error("{0}")]
    Timeout(#[from] crate::database::verification::Timeout),
    #[error("the identity number is occupied")]
    IdNumberTaken,
    #[error("{0}")]
    Completion(#[from] crate::kernel::Error<Infallible>),
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        if let Some(cause) = error.as_db_error()
            && cause.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION
            && cause.schema() == Some("identity")
            && cause.table() == Some("accounts")
            && cause.constraint() == Some("accounts_id_number_key")
        {
            return Self::IdNumberTaken;
        }
        Self::Query(error.into())
    }
}

impl Error {
    pub(crate) fn unreliable(&self) -> bool {
        match self {
            Self::History(error) => error.corrupt(),
            Self::Role(error) => crate::runtime::work::Failure::unreliable(error),
            Self::Revision(error) => error.unreliable(),
            Self::Completion(error) => error.is_corrupt(),
            Self::Facts { .. } => true,
            Self::Query(query) => {
                let mut option = error::Error::source(query);
                while let Some(cause) = option {
                    if cause.is::<platform::cryptography::envelope::ParseError>() {
                        return true;
                    }
                    option = cause.source();
                }
                false
            }
            _ => false,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{Canonical, CanonicalOperator, CanonicalTarget};
    use crate::identity::person::{self, name::Name, number::IdNumber, revision};

    #[test]
    fn canonical_input_matches_independent_json_vector() {
        let name: Name = "张三".parse().expect("name");
        let id_number: IdNumber = "11010519491231002X".parse().expect("identity number");
        let canonical = Canonical {
            operator: CanonicalOperator {
                mobile: "13800000000",
            },
            target: CanonicalTarget {
                person_id: "00000000-0000-4000-8000-000000000001"
                    .parse::<person::Id>()
                    .expect("person"),
                revision_id: "00000000-0000-4000-8000-000000000002"
                    .parse::<revision::Id>()
                    .expect("revision"),
            },
            name: &name,
            id_number: &id_number,
        };
        assert_eq!(
            serde_json::to_vec(&canonical).expect("canonical JSON"),
            r#"{"operator":{"mobile":"13800000000"},"target":{"personId":"00000000-0000-4000-8000-000000000001","revisionId":"00000000-0000-4000-8000-000000000002"},"name":"张三","idNumber":"11010519491231002X"}"#.as_bytes(),
            "system correction retains the original operator and every confirmed value"
        );
    }
}
