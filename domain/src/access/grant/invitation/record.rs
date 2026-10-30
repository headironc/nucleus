//! The sealed, continuous history of one invitation and its other owner's acceptance.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use zeroize::Zeroizing;

use crate::{
    access::{
        grant::{
            self,
            invitation::{Capacity, Expiry, Id},
        },
        role,
    },
    history::{self, Error, Keys, Kind as _, State as _, Token, Verified},
    identity::{
        operation,
        person::{self, revision},
    },
    keys::seal::Seal,
};

pub(crate) const CREATIONS: &str = "access.creations";
pub(crate) const ADJUSTMENTS: &str = "access.adjustments";
pub(crate) const WITHDRAWALS: &str = "access.withdrawals";
pub(crate) const ACCEPTANCES: &str = "access.acceptances";
pub(crate) const TRANSITIONS: &str = "access.invitation_transitions";

#[cfg_attr(test, derive(Clone))]
pub(in crate::access) enum Event {
    Creation,
    Adjustment { id: super::adjustment::Id },
    Withdrawal,
    Acceptance { grant_id: grant::Id },
}

pub(crate) struct Record {
    invitation_id: Id,
    sequence: i64,
    data: Data,
}

#[cfg_attr(test, derive(Clone))]
pub(crate) struct Data {
    pub(super) person_id: person::Id,
    pub(super) grant_id: grant::Id,
    pub(super) epoch: i64,
    pub(super) role_id: role::Id,
    pub(super) change_id: role::change::Id,
    pub(super) capacity: Capacity,
    pub(super) expires_at: Expiry,
    pub(super) used: i64,
    pub(super) withdrawn_at: Option<DateTime<Utc>>,
    pub(super) happened_at: DateTime<Utc>,
    pub(super) operator_id: revision::Id,
    pub(super) operation_id: operation::Id,
    pub(super) event: Event,
}

impl Data {
    pub(in crate::access) fn from_state(
        state: &super::state::Data,
        operation_id: operation::Id,
        operator_id: revision::Id,
        event: Event,
    ) -> Self {
        Self {
            person_id: state.creation.person_id,
            grant_id: state.creation.grant_id,
            epoch: state.creation.epoch,
            role_id: state.creation.role_id,
            change_id: state.creation.change_id,
            capacity: state.capacity,
            expires_at: state.expires_at,
            used: state.used,
            withdrawn_at: state.withdrawn_at,
            happened_at: state.changed_at,
            operator_id,
            operation_id,
            event,
        }
    }
}

impl Record {
    pub(super) fn person_id(&self) -> person::Id {
        self.data.person_id
    }
    pub(in crate::access) fn event(&self) -> &Event {
        &self.data.event
    }
    pub(in crate::access) fn capacity(&self) -> Capacity {
        self.data.capacity
    }
    pub(in crate::access) fn expires_at(&self) -> Expiry {
        self.data.expires_at
    }
    pub(in crate::access) fn happened_at(&self) -> DateTime<Utc> {
        self.data.happened_at
    }
    pub(in crate::access) fn operator_id(&self) -> revision::Id {
        self.data.operator_id
    }
    pub(in crate::access) fn invitation_id(&self) -> Id {
        self.invitation_id
    }

    pub(super) fn matches(&self, state: &super::state::State) -> bool {
        self.invitation_id == state.id()
            && self.data.person_id == state.person_id()
            && self.data.grant_id == state.grant_id()
            && self.data.epoch == state.epoch()
            && self.data.role_id == state.role_id()
            && self.data.change_id == state.change_id()
            && self.data.capacity == state.capacity()
            && self.data.expires_at == state.expires_at()
            && self.data.used == state.used()
            && self.data.withdrawn_at == state.withdrawn_at()
            && self.data.happened_at == state.changed_at()
            && match (&self.data.event, state.sequence()) {
                (Event::Creation, 1) => true,
                (Event::Creation, _) => false,
                (_, 1) => false,
                (_, _) => true,
            }
    }
}

pub(super) async fn original(
    client: &Client,
    roles: &role::Reader,
    family: crate::kernel::operation::Family,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<Verified<Record>>, Error> {
    use crate::kernel::operation::Family;
    let kind = match family {
        Family::Invitation => "creation",
        Family::InvitationAdjustment => "adjustment",
        Family::InvitationWithdrawal => "withdrawal",
        Family::Grant => "acceptance",
        _ => {
            return Err(Error::Integrity(
                "an invitation original uses its declared completion family",
            ));
        }
    };
    client
        .query_opt(
            &format!(
                "{} WHERE record.kind=$1 AND record.operation_id=$2",
                Record::SELECT
            ),
            &[&kind, &operation_id],
        )
        .await?
        .map(|row| {
            let record = history::record::<Record>(&row, roles.keys(), now)?;
            roles.anchors().confirm::<super::state::State>(&record)?;
            Ok(record)
        })
        .transpose()
}

pub(super) async fn page(
    client: &Client,
    keys: Keys<'_>,
    cursor: Option<Id>,
    sequence: i64,
    now: DateTime<Utc>,
) -> Result<Vec<Verified<Record>>, Error> {
    client.query(&format!("{} WHERE $1::uuid IS NULL OR (record.invitation_id,record.sequence)>($1,$2) ORDER BY record.invitation_id,record.sequence LIMIT 128", Record::SELECT), &[&cursor, &sequence]).await?
        .iter().map(|row| history::record(row, keys, now)).collect()
}

pub(super) async fn check_acceptance_pair(client: &Client, id: grant::Id) -> Result<(), Error> {
    let transaction: String = client
        .query_one(
            "SELECT transaction_id::text FROM access.acceptances WHERE id=$1",
            &[&id],
        )
        .await?
        .try_get(0)
        .map_err(Error::decode)?;
    if !grant::record::acceptance_pair(client, id, &transaction).await? {
        return Err(Error::Integrity(
            "an acceptance retains its same-transaction Grant and first assignment",
        ));
    }
    Ok(())
}

impl history::Kind for Record {
    type Error = Error;
    const NAME: &'static str = "access.invitation_record";
    const SELECT: &'static str = r"SELECT record.invitation_id,record.person_id,record.grant_id,record.epoch,record.role_id,record.change_id,record.sequence,record.kind,record.event_id,record.capacity,record.expires_at,record.used,record.withdrawn_at,record.happened_at,record.operator_id,record.operation_id,record.paired,record.seal,record.seal_key_id,record.seal_version FROM (
        SELECT creation.id AS invitation_id,invitation.person_id,invitation.grant_id,invitation.epoch,invitation.role_id,invitation.change_id,creation.sequence,'creation' AS kind,creation.id AS event_id,creation.capacity,creation.expires_at,0::bigint AS used,NULL::timestamptz AS withdrawn_at,creation.created_at AS happened_at,creation.operator_id,creation.operation_id,
            ROW(creation.capacity,creation.expires_at,creation.created_at,creation.operator_id,creation.operation_id,creation.transaction_id)
                IS NOT DISTINCT FROM ROW(invitation.capacity,invitation.expires_at,invitation.created_at,invitation.operator_id,invitation.operation_id,invitation.transaction_id) AS paired,
            creation.seal,creation.seal_key_id,creation.seal_version FROM access.creations creation LEFT JOIN access.invitations invitation ON invitation.id=creation.id
        UNION ALL
        SELECT adjustment.invitation_id,invitation.person_id,invitation.grant_id,invitation.epoch,invitation.role_id,invitation.change_id,adjustment.sequence,'adjustment',adjustment.id,adjustment.capacity,adjustment.expires_at,adjustment.used,NULL::timestamptz,adjustment.adjusted_at,adjustment.operator_id,adjustment.operation_id,true,adjustment.seal,adjustment.seal_key_id,adjustment.seal_version FROM access.adjustments adjustment LEFT JOIN access.invitations invitation ON invitation.id=adjustment.invitation_id
        UNION ALL
        SELECT withdrawal.id,invitation.person_id,invitation.grant_id,invitation.epoch,invitation.role_id,invitation.change_id,withdrawal.sequence,'withdrawal',withdrawal.id,withdrawal.capacity,withdrawal.expires_at,withdrawal.used,withdrawal.withdrawn_at,withdrawal.withdrawn_at,withdrawal.operator_id,withdrawal.operation_id,true,withdrawal.seal,withdrawal.seal_key_id,withdrawal.seal_version FROM access.withdrawals withdrawal LEFT JOIN access.invitations invitation ON invitation.id=withdrawal.id
        UNION ALL
        SELECT acceptance.invitation_id,invitation.person_id,invitation.grant_id,invitation.epoch,invitation.role_id,invitation.change_id,acceptance.sequence,'acceptance',acceptance.id,acceptance.capacity,acceptance.expires_at,acceptance.used,NULL::timestamptz,acceptance.accepted_at,acceptance.operator_id,acceptance.operation_id,true,
            acceptance.seal,acceptance.seal_key_id,acceptance.seal_version FROM access.acceptances acceptance LEFT JOIN access.invitations invitation ON invitation.id=acceptance.invitation_id
    ) record";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let sequence: i64 = row.try_get("sequence").map_err(Error::decode)?;
        let epoch: i64 = row.try_get("epoch").map_err(Error::decode)?;
        let used: i64 = row.try_get("used").map_err(Error::decode)?;
        let happened_at: DateTime<Utc> = row.try_get("happened_at").map_err(Error::decode)?;
        let capacity: Capacity = row.try_get("capacity").map_err(Error::decode)?;
        let expires_at: Expiry = row.try_get("expires_at").map_err(Error::decode)?;
        let withdrawn_at: Option<DateTime<Utc>> =
            row.try_get("withdrawn_at").map_err(Error::decode)?;
        if sequence < 1
            || epoch < 0
            || used < 0
            || i64::from(capacity) < used
            || happened_at > now
            || !row.try_get::<_, bool>("paired").map_err(Error::decode)?
        {
            return Err(Error::Integrity(
                "an invitation record retains its sequence, counters, time and same-transaction owner",
            ));
        }
        let kind: &str = row.try_get("kind").map_err(Error::decode)?;
        let event = match (kind, sequence) {
            ("creation", 1) if used == 0 && withdrawn_at.is_none() => Event::Creation,
            ("adjustment", 2..) if withdrawn_at.is_none() => Event::Adjustment {
                id: row.try_get("event_id").map_err(Error::decode)?,
            },
            ("withdrawal", 2..) if withdrawn_at == Some(happened_at) => Event::Withdrawal,
            ("acceptance", 2..) if withdrawn_at.is_none() && used > 0 => Event::Acceptance {
                grant_id: row.try_get("event_id").map_err(Error::decode)?,
            },
            _ => {
                return Err(Error::Integrity(
                    "an invitation record has a declared event and legal projection",
                ));
            }
        };
        if DateTime::<Utc>::from(expires_at) <= happened_at {
            return Err(Error::Integrity(
                "every invitation record retains expiry after its event",
            ));
        }
        Ok(Self {
            invitation_id: row.try_get("invitation_id").map_err(Error::decode)?,
            sequence,
            data: Data {
                person_id: row.try_get("person_id").map_err(Error::decode)?,
                grant_id: row.try_get("grant_id").map_err(Error::decode)?,
                epoch,
                role_id: row.try_get("role_id").map_err(Error::decode)?,
                change_id: row.try_get("change_id").map_err(Error::decode)?,
                capacity,
                expires_at,
                used,
                withdrawn_at,
                happened_at,
                operator_id: row.try_get("operator_id").map_err(Error::decode)?,
                operation_id: row.try_get("operation_id").map_err(Error::decode)?,
                event,
            },
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let (kind, event_id) = match &self.data.event {
            Event::Creation => ("creation", None),
            Event::Adjustment { id } => ("adjustment", Some(id.as_bytes().to_vec())),
            Event::Withdrawal => ("withdrawal", Some(self.invitation_id.as_bytes().to_vec())),
            Event::Acceptance { grant_id } => ("acceptance", Some(grant_id.as_bytes().to_vec())),
        };
        Zeroizing::new(vec![
            self.invitation_id.as_bytes().to_vec(),
            self.data.person_id.as_bytes().to_vec(),
            self.data.grant_id.as_bytes().to_vec(),
            self.data.epoch.to_string().into_bytes(),
            self.data.role_id.as_bytes().to_vec(),
            self.data.change_id.as_bytes().to_vec(),
            self.sequence.to_string().into_bytes(),
            kind.as_bytes().to_vec(),
            event_id.unwrap_or_default(),
            i64::from(self.data.capacity).to_string().into_bytes(),
            DateTime::<Utc>::from(self.data.expires_at)
                .timestamp_micros()
                .to_string()
                .into_bytes(),
            self.data.used.to_string().into_bytes(),
            self.data
                .withdrawn_at
                .map(|at| at.timestamp_micros().to_string().into_bytes())
                .unwrap_or_default(),
            self.data
                .happened_at
                .timestamp_micros()
                .to_string()
                .into_bytes(),
            self.data.operator_id.as_bytes().to_vec(),
            self.data.operation_id.as_bytes().to_vec(),
        ])
    }
}

impl history::Record for Record {
    const OBJECT: &'static str = "invitation_id";
    type Object = Id;
    type Data = Data;
    fn build(_: Token, invitation_id: Id, sequence: i64, data: Data) -> Self {
        Self {
            invitation_id,
            sequence,
            data,
        }
    }
    fn object(&self) -> Id {
        self.invitation_id
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
        if self.invitation_id != previous.invitation_id
            || after.person_id != before.person_id
            || after.grant_id != before.grant_id
            || after.epoch != before.epoch
            || after.role_id != before.role_id
            || after.change_id != before.change_id
            || after.happened_at < before.happened_at
            || before.withdrawn_at.is_some()
        {
            return false;
        }
        match &after.event {
            Event::Creation => false,
            Event::Adjustment { .. } => {
                after.used == before.used
                    && after.withdrawn_at.is_none()
                    && (after.capacity, after.expires_at) != (before.capacity, before.expires_at)
            }
            Event::Withdrawal => {
                after.withdrawn_at == Some(after.happened_at)
                    && after.capacity == before.capacity
                    && after.expires_at == before.expires_at
                    && after.used == before.used
            }
            Event::Acceptance { .. } => {
                before.used.checked_add(1) == Some(after.used)
                    && after.withdrawn_at.is_none()
                    && after.capacity == before.capacity
                    && after.expires_at == before.expires_at
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
            Event::Creation => {
                client.execute("INSERT INTO access.creations (id,sequence,capacity,expires_at,created_at,operator_id,operation_id,seal,seal_key_id,seal_version) VALUES ($1,1,$2,$3,$4,$5,$6,$7,$8,$9)", &[&self.invitation_id, &data.capacity, &data.expires_at, &data.happened_at, &data.operator_id, &data.operation_id, &seal.tag(), &seal.key_id(), &seal.version()]).await?;
            }
            Event::Adjustment { id } => {
                client.execute("INSERT INTO access.adjustments (id,invitation_id,sequence,capacity,expires_at,used,adjusted_at,operator_id,operation_id,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)", &[&id, &self.invitation_id, &self.sequence, &data.capacity, &data.expires_at, &data.used, &data.happened_at, &data.operator_id, &data.operation_id, &seal.tag(), &seal.key_id(), &seal.version()]).await?;
            }
            Event::Withdrawal => {
                client.execute("INSERT INTO access.withdrawals (id,sequence,capacity,expires_at,used,withdrawn_at,operator_id,operation_id,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)", &[&self.invitation_id, &self.sequence, &data.capacity, &data.expires_at, &data.used, &data.withdrawn_at, &data.operator_id, &data.operation_id, &seal.tag(), &seal.key_id(), &seal.version()]).await?;
            }
            Event::Acceptance { grant_id } => {
                client.execute("INSERT INTO access.acceptances (id,invitation_id,sequence,capacity,expires_at,used,accepted_at,operator_id,operation_id,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)", &[&grant_id, &self.invitation_id, &self.sequence, &data.capacity, &data.expires_at, &data.used, &data.happened_at, &data.operator_id, &data.operation_id, &seal.tag(), &seal.key_id(), &seal.version()]).await?;
            }
        }
        Ok(())
    }
}
