//! Permanent allocation, correction, recovery and revocation records in object order.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    history::{self, Error, Keys, Kind, Token},
    identity::{operation, person},
    keys::seal::Seal,
    workforce::number::{Id, Reason, Source},
};

pub(crate) const ALLOCATIONS: &str = "workforce.allocations";
pub(crate) const CORRECTIONS: &str = "workforce.corrections";
pub(crate) const RECOVERIES: &str = "workforce.recoveries";
pub(crate) const REVOCATIONS: &str = "workforce.revocations";
pub(crate) const TRANSITIONS: &str = "workforce.transitions";

pub(super) enum Event {
    Allocation {
        source: Source,
        reason: Option<Reason>,
    },
    Correction {
        before_id: Id,
        reason: Reason,
    },
    Recovery {
        origin_id: Id,
        occupier_id: Uuid,
        revocation_id: Option<Uuid>,
        reason: Reason,
    },
    Revocation {
        recovery_id: Uuid,
        reason: Reason,
    },
}

impl Event {
    fn label(&self) -> &'static str {
        match self {
            Self::Allocation { .. } => "allocation",
            Self::Correction { .. } => "correction",
            Self::Recovery { .. } => "recovery",
            Self::Revocation { .. } => "revocation",
        }
    }
}

pub(super) fn source(source: Source) -> &'static str {
    match source {
        Source::SelfService => "self_service",
        Source::Assistance => "assistance",
        Source::Supplement => "supplement",
    }
}

pub(super) struct Data {
    pub(super) id: Uuid,
    pub(super) revision_id: person::revision::Id,
    pub(super) number_id: Id,
    pub(super) operator_id: person::revision::Id,
    pub(super) time: DateTime<Utc>,
    pub(super) operation_id: operation::Id,
    pub(super) tenure_id: Uuid,
    pub(super) held: bool,
    pub(super) event: Event,
}

pub(super) struct Record {
    person_id: person::Id,
    sequence: i64,
    data: Data,
}

impl Record {
    pub(super) fn id(&self) -> Uuid {
        self.data.id
    }
    pub(super) fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub(super) fn revision_id(&self) -> person::revision::Id {
        self.data.revision_id
    }
    pub(super) fn number_id(&self) -> Id {
        self.data.number_id
    }
    pub(super) fn operator_id(&self) -> person::revision::Id {
        self.data.operator_id
    }
    pub(super) fn time(&self) -> DateTime<Utc> {
        self.data.time
    }
    pub(super) fn tenure_id(&self) -> Uuid {
        self.data.tenure_id
    }
    pub(super) fn held(&self) -> bool {
        self.data.held
    }
    pub(super) fn event(&self) -> &Event {
        &self.data.event
    }
}

fn reason(row: &Row) -> Result<Reason, Error> {
    let stored = row.try_get::<_, String>("reason").map_err(Error::decode)?;
    let reason = Reason::try_from(stored.clone())
        .map_err(|_| Error::Integrity("number record retains a valid reason"))?;
    if reason.as_ref() != stored {
        return Err(Error::Integrity(
            "number record retains its canonical reason",
        ));
    }
    Ok(reason)
}

impl Kind for Record {
    type Error = Error;
    const NAME: &'static str = "workforce.number.record";
    const SELECT: &'static str = "SELECT record.kind,record.id,record.person_id,record.revision_id,record.number_id,record.operator_id,record.happened_at,record.sequence,record.operation_id,record.tenure_id,record.held,record.source,record.reason,record.before_id,record.origin_id,record.occupier_id,record.revocation_id,record.recovery_id,record.seal,record.seal_key_id,record.seal_version FROM workforce.transitions record";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let event = match row.try_get::<_, &str>("kind").map_err(Error::decode)? {
            "allocation" => {
                let source = match row.try_get::<_, &str>("source").map_err(Error::decode)? {
                    "self_service" => Source::SelfService,
                    "assistance" => Source::Assistance,
                    "supplement" => Source::Supplement,
                    _ => return Err(Error::Integrity("allocation source is closed")),
                };
                let reason = if source == Source::SelfService {
                    if row
                        .try_get::<_, Option<String>>("reason")
                        .map_err(Error::decode)?
                        .is_some()
                    {
                        return Err(Error::Integrity("self-service allocation has no reason"));
                    }
                    None
                } else {
                    Some(reason(row)?)
                };
                Event::Allocation { source, reason }
            }
            "correction" => Event::Correction {
                before_id: row.try_get("before_id").map_err(Error::decode)?,
                reason: reason(row)?,
            },
            "recovery" => Event::Recovery {
                origin_id: row.try_get("origin_id").map_err(Error::decode)?,
                occupier_id: row.try_get("occupier_id").map_err(Error::decode)?,
                revocation_id: row.try_get("revocation_id").map_err(Error::decode)?,
                reason: reason(row)?,
            },
            "revocation" => Event::Revocation {
                recovery_id: row.try_get("recovery_id").map_err(Error::decode)?,
                reason: reason(row)?,
            },
            _ => return Err(Error::Integrity("number record kind is closed")),
        };
        let time = row.try_get("happened_at").map_err(Error::decode)?;
        let sequence = row.try_get("sequence").map_err(Error::decode)?;
        if time > now || sequence < 1 {
            return Err(Error::Integrity(
                "number record has a positive sequence and non-future time",
            ));
        }
        Ok(Self {
            person_id: row.try_get("person_id").map_err(Error::decode)?,
            sequence,
            data: Data {
                id: row.try_get("id").map_err(Error::decode)?,
                revision_id: row.try_get("revision_id").map_err(Error::decode)?,
                number_id: row.try_get("number_id").map_err(Error::decode)?,
                operator_id: row.try_get("operator_id").map_err(Error::decode)?,
                time,
                operation_id: row.try_get("operation_id").map_err(Error::decode)?,
                tenure_id: row.try_get("tenure_id").map_err(Error::decode)?,
                held: row.try_get("held").map_err(Error::decode)?,
                event,
            },
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let mut fields = vec![
            self.event().label().as_bytes().to_vec(),
            self.id().as_bytes().to_vec(),
            self.person_id().as_bytes().to_vec(),
            self.revision_id().as_bytes().to_vec(),
            self.number_id().as_bytes().to_vec(),
            self.operator_id().as_bytes().to_vec(),
            self.time().timestamp_micros().to_string().into_bytes(),
            self.sequence.to_string().into_bytes(),
            self.data.operation_id.as_bytes().to_vec(),
            self.tenure_id().as_bytes().to_vec(),
            self.held().to_string().into_bytes(),
        ];
        match self.event() {
            Event::Allocation {
                source: kind,
                reason,
            } => {
                fields.push(source(*kind).as_bytes().to_vec());
                fields.push(
                    reason
                        .as_ref()
                        .map(|reason| reason.as_ref().as_bytes().to_vec())
                        .unwrap_or_default(),
                );
            }
            Event::Correction { before_id, reason } => {
                fields.push(before_id.as_bytes().to_vec());
                fields.push(reason.as_ref().as_bytes().to_vec());
            }
            Event::Recovery {
                origin_id,
                occupier_id,
                revocation_id,
                reason,
            } => {
                fields.push(origin_id.as_bytes().to_vec());
                fields.push(occupier_id.as_bytes().to_vec());
                fields.push(
                    revocation_id
                        .map(|id| id.as_bytes().to_vec())
                        .unwrap_or_default(),
                );
                fields.push(reason.as_ref().as_bytes().to_vec());
            }
            Event::Revocation {
                recovery_id,
                reason,
            } => {
                fields.push(recovery_id.as_bytes().to_vec());
                fields.push(reason.as_ref().as_bytes().to_vec());
            }
        }
        Zeroizing::new(fields)
    }
}

impl history::Record for Record {
    const OBJECT: &'static str = "person_id";
    type Object = person::Id;
    type Data = Data;
    fn build(_: Token, person_id: person::Id, sequence: i64, data: Data) -> Self {
        Self {
            person_id,
            sequence,
            data,
        }
    }
    fn object(&self) -> person::Id {
        self.person_id
    }
    fn sequence(&self) -> i64 {
        self.sequence
    }
    fn operation_id(&self) -> operation::Id {
        self.data.operation_id
    }
    async fn insert(
        &self,
        _: Token,
        client: &Client,
        _: Keys<'_>,
        seal: &Seal,
    ) -> Result<(), Error> {
        let common: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[
            &self.data.id,
            &self.person_id,
            &self.data.revision_id,
            &self.data.number_id,
            &self.data.operator_id,
            &self.data.time,
            &self.sequence,
            &self.data.operation_id,
            &self.data.tenure_id,
            &self.data.held,
            &seal.tag(),
            &seal.key_id(),
            &seal.version(),
        ];
        let mut params = common.to_vec();
        match self.event() {
            Event::Allocation {
                source: kind,
                reason,
            } => {
                let source = source(*kind);
                let reason = reason.as_ref().map(AsRef::<str>::as_ref);
                params.push(&source);
                params.push(&reason);
                client.execute("INSERT INTO workforce.allocations (id,person_id,revision_id,number_id,operator_id,allocated_at,sequence,operation_id,tenure_id,held,seal,seal_key_id,seal_version,source,reason) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",&params).await?;
            }
            Event::Correction { before_id, reason } => {
                let reason = reason.as_ref();
                params.push(before_id);
                params.push(&reason);
                client.execute("INSERT INTO workforce.corrections (id,person_id,revision_id,after_id,operator_id,corrected_at,sequence,operation_id,tenure_id,held,seal,seal_key_id,seal_version,before_id,reason) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",&params).await?;
            }
            Event::Recovery {
                origin_id,
                occupier_id,
                revocation_id,
                reason,
            } => {
                let reason = reason.as_ref();
                params.push(origin_id);
                params.push(occupier_id);
                params.push(revocation_id);
                params.push(&reason);
                client.execute("INSERT INTO workforce.recoveries (id,person_id,revision_id,number_id,operator_id,recovered_at,sequence,operation_id,tenure_id,held,seal,seal_key_id,seal_version,origin_id,occupier_id,revocation_id,reason) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",&params).await?;
            }
            Event::Revocation {
                recovery_id,
                reason,
            } => {
                let reason = reason.as_ref();
                params.push(recovery_id);
                params.push(&reason);
                client.execute("INSERT INTO workforce.revocations (id,person_id,revision_id,number_id,operator_id,revoked_at,sequence,operation_id,tenure_id,held,seal,seal_key_id,seal_version,recovery_id,reason) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",&params).await?;
            }
        }
        Ok(())
    }
}

pub(super) async fn by_id(
    client: &Client,
    keys: Keys<'_>,
    kind: &str,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<history::Verified<Record>, Error> {
    let row = client
        .query_opt(
            &format!("{} WHERE record.kind=$1 AND record.id=$2", Record::SELECT),
            &[&kind, &id],
        )
        .await?
        .ok_or(Error::Integrity(
            "an original result retains its fixed record",
        ))?;
    history::record(&row, keys, now)
}

pub(super) async fn batch(
    client: &Client,
    keys: Keys<'_>,
    person: Option<person::Id>,
    sequence: i64,
    now: DateTime<Utc>,
) -> Result<Vec<history::Verified<Record>>, Error> {
    client.query(&format!("{} WHERE $1::uuid IS NULL OR (record.person_id,record.sequence)>($1,$2) ORDER BY record.person_id,record.sequence LIMIT 128", Record::SELECT), &[&person,&sequence]).await?
        .iter().map(|row| history::record(row, keys, now)).collect()
}

pub(super) async fn original(
    client: &Client,
    keys: Keys<'_>,
    kind: &str,
    operation: operation::Id,
    now: DateTime<Utc>,
) -> Result<history::Verified<Record>, Error> {
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.kind=$1 AND record.operation_id=$2",
                Record::SELECT
            ),
            &[&kind, &operation],
        )
        .await?
        .ok_or(Error::Integrity(
            "a completion retains its original number record",
        ))?;
    history::record(&row, keys, now)
}
