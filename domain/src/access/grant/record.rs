//! The sealed, continuous history of one Grant, including projections from other owners.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    access::{
        Entry, assignment,
        grant::{
            Id,
            state::{self, State},
            suspension,
            termination::Mode,
        },
        role,
    },
    history::{self, Error, Keys, Token},
    identity::{
        cancellation, operation,
        person::{self, revision},
    },
    keys::seal::Seal,
};

pub(crate) const ASSIGNMENTS: &str = "access.assignments";
pub(crate) const SUSPENSIONS: &str = "access.suspensions";
pub(crate) const RESUMPTIONS: &str = "access.resumptions";
pub(crate) const TERMINATIONS: &str = "access.terminations";
pub(crate) const LOSSES: &str = "access.losses";

/// This hint chooses locks; the command still authenticates the resulting Grant state.
pub(super) async fn suspension_grant(
    client: &Client,
    id: suspension::Id,
) -> Result<Option<Id>, Error> {
    client
        .query_opt(
            "SELECT grant_id FROM access.suspensions WHERE id=$1",
            &[&id],
        )
        .await?
        .map(|row| row.try_get("grant_id").map_err(Error::decode))
        .transpose()
}

pub(super) async fn acceptance_pair(
    client: &Client,
    id: Id,
    transaction: &str,
) -> Result<bool, Error> {
    client.query_one("SELECT EXISTS (SELECT 1 FROM access.grants admission JOIN access.assignments first ON first.grant_id=admission.id AND first.sequence=1 WHERE admission.id=$1 AND admission.entry='invitation' AND admission.invitation_id IS NOT NULL AND admission.transaction_id::text=$2 AND first.transaction_id=admission.transaction_id)", &[&id, &transaction])
        .await?.try_get(0).map_err(Error::decode)
}

pub(in crate::access) async fn confirm_losses(
    client: &Client,
    roles: &role::Reader,
    change_id: role::change::Id,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    use history::Kind as _;
    let rows = client
        .query(
            &format!(
                "{} WHERE record.kind='loss' AND record.operation_id=$1",
                Record::SELECT
            ),
            &[&operation_id],
        )
        .await?;
    for row in rows {
        let record = history::record::<Record>(&row, roles.keys(), now)?;
        if !matches!(record.event(), Event::Loss { change_id: cause, .. } if *cause==change_id) {
            return Err(Error::Integrity(
                "a role operation retains exactly its own loss projections",
            ));
        }
        roles.anchors().confirm::<State>(&record)?;
    }
    Ok(())
}

#[cfg_attr(test, derive(Clone))]
pub(in crate::access) enum Event {
    Assignment {
        previous_id: Option<assignment::Id>,
        change_id: role::change::Id,
        entry: Option<Entry>,
        operator_id: revision::Id,
        revision_id: revision::Id,
    },
    Suspension {
        id: suspension::Id,
        operator_id: revision::Id,
        revision_id: revision::Id,
    },
    Resumption {
        id: suspension::Id,
        operator_id: revision::Id,
        revision_id: revision::Id,
    },
    Termination {
        mode: Mode,
        operator_id: revision::Id,
        revision_id: revision::Id,
        cancellation_id: Option<cancellation::Id>,
    },
    Loss {
        id: Uuid,
        change_id: role::change::Id,
    },
}

pub(crate) struct Record {
    grant_id: Id,
    sequence: i64,
    data: Data,
}

#[cfg_attr(test, derive(Clone))]
pub(crate) struct Data {
    pub(super) person_id: person::Id,
    pub(super) role_id: role::Id,
    pub(super) assignment_id: assignment::Id,
    pub(super) suspension_id: Option<suspension::Id>,
    pub(super) termination: Option<Mode>,
    pub(super) epoch: i64,
    pub(super) happened_at: DateTime<Utc>,
    pub(super) operation_id: operation::Id,
    pub(super) event: Event,
}

impl Data {
    pub(super) fn new(state: &state::Data, operation_id: operation::Id, event: Event) -> Self {
        Self {
            person_id: state.admission.person_id,
            role_id: state.role_id,
            assignment_id: state.assignment_id,
            suspension_id: state.suspension_id,
            termination: state.termination,
            epoch: state.epoch,
            happened_at: state.changed_at,
            operation_id,
            event,
        }
    }
}

impl Record {
    pub(in crate::access) fn grant_id(&self) -> Id {
        self.grant_id
    }
    pub(in crate::access) fn person_id(&self) -> person::Id {
        self.data.person_id
    }
    pub(in crate::access) fn role_id(&self) -> role::Id {
        self.data.role_id
    }
    pub(in crate::access) fn assignment_id(&self) -> assignment::Id {
        self.data.assignment_id
    }
    pub(in crate::access) fn happened_at(&self) -> DateTime<Utc> {
        self.data.happened_at
    }
    pub(in crate::access) fn event(&self) -> &Event {
        &self.data.event
    }

    pub(super) fn matches(&self, state: &State) -> bool {
        self.grant_id == state.id()
            && self.data.person_id == state.person_id()
            && self.data.role_id == state.role_id()
            && self.data.assignment_id == state.assignment_id()
            && self.data.suspension_id == state.suspension_id()
            && self.data.termination == state.termination()
            && self.data.epoch == state.epoch()
            && self.data.happened_at == state.changed_at()
    }
}

#[cfg(test)]
pub(super) async fn retime(
    client: &Client,
    keys: Keys<'_>,
    operation: operation::Id,
    at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    use crate::history::{Record as _, Seed};
    let record = history::original::<Record>(client, keys, operation, now)
        .await?
        .ok_or(Error::Integrity("the ordering fixture retains its record"))?;
    let mut data = record.data.clone();
    data.happened_at = at;
    let updated = Seed::record::<Record>(record.grant_id, record.sequence(), data, keys)?;
    let seal = updated.seal();
    let (table, time) = match updated.event() {
        Event::Assignment { .. } => ("access.assignments", "assigned_at"),
        Event::Suspension { .. } => ("access.suspensions", "suspended_at"),
        Event::Resumption { .. } => ("access.resumptions", "resumed_at"),
        Event::Termination {
            mode: Mode::Cancellation,
            ..
        } => {
            return Err(Error::Integrity(
                "a cancellation ordering fixture keeps its original source time",
            ));
        }
        Event::Termination { .. } => ("access.terminations", "terminated_at"),
        Event::Loss { .. } => {
            return Err(Error::Integrity(
                "the ordering fixture does not retime role projections",
            ));
        }
    };
    client.execute(&format!("UPDATE {table} SET {time}=$2,seal=$3,seal_key_id=$4,seal_version=$5 WHERE operation_id=$1"), &[&operation,&at,&seal.tag(),&seal.key_id(),&seal.version()]).await?;
    Ok(())
}

impl history::Kind for Record {
    type Error = Error;
    const NAME: &'static str = "access.grant_record";
    const SELECT: &'static str = r"SELECT record.grant_id,record.person_id,record.role_id,record.assignment_id,record.suspension_id,record.termination,record.epoch,record.happened_at,record.sequence,record.operation_id,record.kind,record.id,record.previous_id,record.change_id,record.entry,record.operator_id,record.revision_id,record.cancellation_id,record.paired,record.seal,record.seal_key_id,record.seal_version FROM (
        SELECT assignment.grant_id,assignment.person_id,assignment.role_id,assignment.id AS assignment_id,assignment.suspension_id,NULL::text AS termination,assignment.epoch,assignment.assigned_at AS happened_at,assignment.sequence,assignment.operation_id,'assignment' AS kind,assignment.id,assignment.previous_id,assignment.change_id,assignment.entry,assignment.operator_id,assignment.revision_id,NULL::uuid AS cancellation_id,
            CASE WHEN assignment.sequence=1 THEN EXISTS (SELECT 1 FROM access.grants admission WHERE admission.id=assignment.grant_id AND admission.person_id=assignment.person_id AND admission.operator_id=assignment.operator_id AND admission.revision_id=assignment.revision_id AND admission.granted_at=assignment.assigned_at AND admission.operation_id=assignment.operation_id AND admission.transaction_id=assignment.transaction_id) ELSE true END AS paired,
            assignment.seal,assignment.seal_key_id,assignment.seal_version FROM access.assignments assignment
        UNION ALL
        SELECT suspension.grant_id,suspension.person_id,suspension.role_id,suspension.assignment_id,suspension.id,NULL::text,suspension.epoch,suspension.suspended_at,suspension.sequence,suspension.operation_id,'suspension',suspension.id,NULL::uuid,NULL::uuid,NULL::text,suspension.operator_id,suspension.revision_id,NULL::uuid,true,suspension.seal,suspension.seal_key_id,suspension.seal_version FROM access.suspensions suspension
        UNION ALL
        SELECT resumption.grant_id,resumption.person_id,resumption.role_id,resumption.assignment_id,NULL::uuid,NULL::text,resumption.epoch,resumption.resumed_at,resumption.sequence,resumption.operation_id,'resumption',resumption.id,NULL::uuid,NULL::uuid,NULL::text,resumption.operator_id,resumption.revision_id,NULL::uuid,true,resumption.seal,resumption.seal_key_id,resumption.seal_version FROM access.resumptions resumption
        UNION ALL
        SELECT termination.id,termination.person_id,termination.role_id,termination.assignment_id,termination.suspension_id,termination.mode,termination.epoch,termination.terminated_at,termination.sequence,termination.operation_id,'termination',termination.id,NULL::uuid,NULL::uuid,NULL::text,termination.operator_id,termination.revision_id,termination.cancellation_id,
            CASE WHEN termination.mode='cancellation' THEN EXISTS (SELECT 1 FROM identity.changes cause WHERE cause.kind='cancellation' AND cause.id=termination.cancellation_id AND cause.person_id=termination.person_id AND cause.operator_id=termination.operator_id AND cause.revision_id=termination.revision_id AND cause.happened_at=termination.terminated_at AND cause.operation_id=termination.operation_id AND cause.transaction_id=termination.transaction_id) ELSE true END,
            termination.seal,termination.seal_key_id,termination.seal_version FROM access.terminations termination
        UNION ALL
        SELECT loss.grant_id,loss.person_id,loss.role_id,loss.assignment_id,loss.suspension_id,NULL::text,loss.epoch,loss.projected_at,loss.sequence,loss.operation_id,'loss',loss.id,NULL::uuid,loss.change_id,NULL::text,NULL::uuid,NULL::uuid,NULL::uuid,access.loss_valid(loss.id),loss.seal,loss.seal_key_id,loss.seal_version FROM access.losses loss
    ) record";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let sequence: i64 = row.try_get("sequence").map_err(Error::decode)?;
        let epoch: i64 = row.try_get("epoch").map_err(Error::decode)?;
        let happened_at = row.try_get("happened_at").map_err(Error::decode)?;
        if sequence < 1
            || epoch < 0
            || happened_at > now
            || !row.try_get::<_, bool>("paired").map_err(Error::decode)?
        {
            return Err(Error::Integrity(
                "a grant record retains its sequence, time and same-transaction cause",
            ));
        }
        let termination =
            state::parse_termination(row.try_get("termination").map_err(Error::decode)?)?;
        let suspension_id: Option<suspension::Id> =
            row.try_get("suspension_id").map_err(Error::decode)?;
        let event = match row.try_get::<_, &str>("kind").map_err(Error::decode)? {
            "assignment" => {
                let previous_id: Option<assignment::Id> =
                    row.try_get("previous_id").map_err(Error::decode)?;
                let entry: Option<Entry> = row.try_get("entry").map_err(Error::decode)?;
                if (sequence == 1) != previous_id.is_none()
                    || (sequence == 1) != entry.is_none()
                    || (sequence == 1 && (epoch != 0 || suspension_id.is_some()))
                    || termination.is_some()
                {
                    return Err(Error::Integrity(
                        "a grant begins with its first assignment at epoch zero",
                    ));
                }
                Event::Assignment {
                    previous_id,
                    entry,
                    change_id: row.try_get("change_id").map_err(Error::decode)?,
                    operator_id: row.try_get("operator_id").map_err(Error::decode)?,
                    revision_id: row.try_get("revision_id").map_err(Error::decode)?,
                }
            }
            "suspension" if sequence > 1 && termination.is_none() => Event::Suspension {
                id: row.try_get("id").map_err(Error::decode)?,
                operator_id: row.try_get("operator_id").map_err(Error::decode)?,
                revision_id: row.try_get("revision_id").map_err(Error::decode)?,
            },
            "resumption" if sequence > 1 && suspension_id.is_none() && termination.is_none() => {
                Event::Resumption {
                    id: row.try_get("id").map_err(Error::decode)?,
                    operator_id: row.try_get("operator_id").map_err(Error::decode)?,
                    revision_id: row.try_get("revision_id").map_err(Error::decode)?,
                }
            }
            "termination" if sequence > 1 => {
                let mode = termination.ok_or(Error::Integrity("a termination retains its mode"))?;
                let cancellation_id: Option<cancellation::Id> =
                    row.try_get("cancellation_id").map_err(Error::decode)?;
                let operator_id = row.try_get("operator_id").map_err(Error::decode)?;
                let revision_id = row.try_get("revision_id").map_err(Error::decode)?;
                if (mode == Mode::Cancellation) != cancellation_id.is_some()
                    || (mode == Mode::Current && operator_id != revision_id)
                    || (mode == Mode::Manager && operator_id == revision_id)
                {
                    return Err(Error::Integrity(
                        "a termination keeps its original human or cancellation source",
                    ));
                }
                Event::Termination {
                    mode,
                    operator_id,
                    revision_id,
                    cancellation_id,
                }
            }
            "loss" if sequence > 1 && epoch > 0 && termination.is_none() => Event::Loss {
                id: row.try_get("id").map_err(Error::decode)?,
                change_id: row.try_get("change_id").map_err(Error::decode)?,
            },
            _ => {
                return Err(Error::Integrity(
                    "a grant record has a declared event and legal state",
                ));
            }
        };
        Ok(Self {
            grant_id: row.try_get("grant_id").map_err(Error::decode)?,
            sequence,
            data: Data {
                person_id: row.try_get("person_id").map_err(Error::decode)?,
                role_id: row.try_get("role_id").map_err(Error::decode)?,
                assignment_id: row.try_get("assignment_id").map_err(Error::decode)?,
                suspension_id,
                termination,
                epoch,
                happened_at,
                operation_id: row.try_get("operation_id").map_err(Error::decode)?,
                event,
            },
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let mut fields = vec![
            self.grant_id.as_bytes().to_vec(),
            self.data.person_id.as_bytes().to_vec(),
            self.data.role_id.as_bytes().to_vec(),
            self.data.assignment_id.as_bytes().to_vec(),
            self.data
                .suspension_id
                .map(|id| id.as_bytes().to_vec())
                .unwrap_or_default(),
            state::termination_label(self.data.termination)
                .unwrap_or_default()
                .as_bytes()
                .to_vec(),
            self.data.epoch.to_string().into_bytes(),
            self.data
                .happened_at
                .timestamp_micros()
                .to_string()
                .into_bytes(),
            self.sequence.to_string().into_bytes(),
            self.data.operation_id.as_bytes().to_vec(),
        ];
        match &self.data.event {
            Event::Assignment {
                previous_id,
                change_id,
                entry,
                operator_id,
                revision_id,
            } => fields.extend([
                b"assignment".to_vec(),
                previous_id
                    .map(|id| id.as_bytes().to_vec())
                    .unwrap_or_default(),
                change_id.as_bytes().to_vec(),
                entry.as_ref().map_or("", AsRef::as_ref).as_bytes().to_vec(),
                operator_id.as_bytes().to_vec(),
                revision_id.as_bytes().to_vec(),
            ]),
            Event::Suspension {
                id,
                operator_id,
                revision_id,
            } => fields.extend([
                b"suspension".to_vec(),
                id.as_bytes().to_vec(),
                operator_id.as_bytes().to_vec(),
                revision_id.as_bytes().to_vec(),
            ]),
            Event::Resumption {
                id,
                operator_id,
                revision_id,
            } => fields.extend([
                b"resumption".to_vec(),
                id.as_bytes().to_vec(),
                operator_id.as_bytes().to_vec(),
                revision_id.as_bytes().to_vec(),
            ]),
            Event::Termination {
                mode,
                operator_id,
                revision_id,
                cancellation_id,
            } => fields.extend([
                b"termination".to_vec(),
                state::termination_label(Some(*mode))
                    .unwrap_or_default()
                    .as_bytes()
                    .to_vec(),
                operator_id.as_bytes().to_vec(),
                revision_id.as_bytes().to_vec(),
                cancellation_id
                    .map(|id| id.as_bytes().to_vec())
                    .unwrap_or_default(),
            ]),
            Event::Loss { id, change_id } => fields.extend([
                b"loss".to_vec(),
                id.as_bytes().to_vec(),
                change_id.as_bytes().to_vec(),
            ]),
        }
        Zeroizing::new(fields)
    }
}

impl history::Record for Record {
    const OBJECT: &'static str = "grant_id";
    type Object = Id;
    type Data = Data;
    fn build(_: Token, grant_id: Id, sequence: i64, data: Data) -> Self {
        Self {
            grant_id,
            sequence,
            data,
        }
    }
    fn object(&self) -> Id {
        self.grant_id
    }
    fn sequence(&self) -> i64 {
        self.sequence
    }
    fn operation_id(&self) -> operation::Id {
        self.data.operation_id
    }
    fn follows(&self, previous: &Self) -> bool {
        let before = &previous.data;
        let after = &self.data;
        if self.grant_id != previous.grant_id
            || after.person_id != before.person_id
            || after.happened_at < before.happened_at
            || before.termination.is_some()
        {
            return false;
        }
        match &after.event {
            Event::Assignment { previous_id, .. } => {
                *previous_id == Some(before.assignment_id)
                    && after.role_id != before.role_id
                    && after.assignment_id != before.assignment_id
                    && after.suspension_id == before.suspension_id
                    && after.termination.is_none()
                    && (after.epoch == before.epoch
                        || before.epoch.checked_add(1) == Some(after.epoch))
            }
            Event::Suspension { id, .. } => {
                after.role_id == before.role_id
                    && after.assignment_id == before.assignment_id
                    && before.suspension_id.is_none()
                    && after.suspension_id == Some(*id)
                    && after.termination.is_none()
                    && after.epoch == before.epoch
            }
            Event::Resumption { id, .. } => {
                after.role_id == before.role_id
                    && after.assignment_id == before.assignment_id
                    && before.suspension_id == Some(*id)
                    && after.suspension_id.is_none()
                    && after.termination.is_none()
                    && after.epoch == before.epoch
            }
            Event::Termination { mode, .. } => {
                after.role_id == before.role_id
                    && after.assignment_id == before.assignment_id
                    && after.suspension_id == before.suspension_id
                    && after.termination == Some(*mode)
                    && after.epoch == before.epoch
            }
            Event::Loss { .. } => {
                after.role_id == before.role_id
                    && after.assignment_id == before.assignment_id
                    && after.suspension_id == before.suspension_id
                    && after.termination.is_none()
                    && before.epoch.checked_add(1) == Some(after.epoch)
            }
        }
    }

    async fn insert(
        &self,
        _: Token,
        client: &Client,
        _: Keys<'_>,
        seal: &Seal,
    ) -> Result<(), Error> {
        let data = &self.data;
        match &data.event {
            Event::Assignment {
                previous_id,
                change_id,
                entry,
                operator_id,
                revision_id,
            } => {
                client.execute("INSERT INTO access.assignments (id,grant_id,person_id,role_id,previous_id,change_id,operator_id,revision_id,assigned_at,sequence,operation_id,entry,suspension_id,epoch,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)", &[&data.assignment_id, &self.grant_id, &data.person_id, &data.role_id, &previous_id, &change_id, &operator_id, &revision_id, &data.happened_at, &self.sequence, &data.operation_id, &entry, &data.suspension_id, &data.epoch, &seal.tag(), &seal.key_id(), &seal.version()]).await?;
            }
            Event::Suspension {
                id,
                operator_id,
                revision_id,
            } => {
                client.execute("INSERT INTO access.suspensions (id,grant_id,person_id,role_id,assignment_id,operator_id,revision_id,suspended_at,sequence,operation_id,epoch,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)", &[&id, &self.grant_id, &data.person_id, &data.role_id, &data.assignment_id, &operator_id, &revision_id, &data.happened_at, &self.sequence, &data.operation_id, &data.epoch, &seal.tag(), &seal.key_id(), &seal.version()]).await?;
            }
            Event::Resumption {
                id,
                operator_id,
                revision_id,
            } => {
                client.execute("INSERT INTO access.resumptions (id,grant_id,person_id,role_id,assignment_id,operator_id,revision_id,resumed_at,sequence,operation_id,epoch,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)", &[&id, &self.grant_id, &data.person_id, &data.role_id, &data.assignment_id, &operator_id, &revision_id, &data.happened_at, &self.sequence, &data.operation_id, &data.epoch, &seal.tag(), &seal.key_id(), &seal.version()]).await?;
            }
            Event::Termination {
                mode,
                operator_id,
                revision_id,
                cancellation_id,
            } => {
                client.execute("INSERT INTO access.terminations (id,person_id,role_id,assignment_id,suspension_id,operator_id,revision_id,terminated_at,sequence,operation_id,epoch,mode,cancellation_id,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)", &[&self.grant_id, &data.person_id, &data.role_id, &data.assignment_id, &data.suspension_id, &operator_id, &revision_id, &data.happened_at, &self.sequence, &data.operation_id, &data.epoch, &state::termination_label(Some(*mode)), &cancellation_id, &seal.tag(), &seal.key_id(), &seal.version()]).await?;
            }
            Event::Loss { id, change_id } => {
                client.execute("INSERT INTO access.losses (id,grant_id,person_id,role_id,assignment_id,suspension_id,epoch,change_id,projected_at,sequence,operation_id,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)", &[&id, &self.grant_id, &data.person_id, &data.role_id, &data.assignment_id, &data.suspension_id, &data.epoch, &change_id, &data.happened_at, &self.sequence, &data.operation_id, &seal.tag(), &seal.key_id(), &seal.version()]).await?;
            }
        }
        Ok(())
    }
}
