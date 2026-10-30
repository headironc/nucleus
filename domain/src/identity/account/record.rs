//! Immutable account transitions and projections of permanent Identity facts.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use platform::cryptography::envelope;
use tokio_postgres::{Client, Row};
use zeroize::Zeroizing;

use crate::{
    database::sealed::Sealed,
    history::{self, Error, Keys, Token},
    identity::{
        account::{Id, state::State},
        cancellation, correction, message,
        mobile::Mobile,
        operation,
        person::{self, revision},
        revocation::Scope,
        session, suspension, transfer,
    },
    keys::seal::Seal,
};

pub(crate) const CREATIONS: &str = "identity.creations";
pub(crate) const SUSPENSIONS: &str = "identity.suspensions";
pub(crate) const RESUMPTIONS: &str = "identity.resumptions";
pub(crate) const REPLACEMENTS: &str = "identity.replacements";
pub(crate) const LOGOUTS: &str = "identity.logouts";
pub(crate) const PROJECTIONS: &str = "identity.projections";
pub(crate) const PORTRAITS: &str = "identity.portraits";

pub(crate) async fn cancellation(
    client: &Client,
    keys: Keys<'_>,
    id: cancellation::Id,
    now: DateTime<Utc>,
) -> Result<history::Verified<Record>, Error> {
    use crate::history::Kind as _;
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.kind='cancellation' AND record.id=$1",
                Record::SELECT
            ),
            &[&id],
        )
        .await?
        .ok_or(Error::Integrity(
            "a cancellation retains its account projection",
        ))?;
    history::record(&row, keys, now)
}

pub(crate) async fn avatar(
    client: &Client,
    keys: Keys<'_>,
    operation: operation::Id,
    now: DateTime<Utc>,
) -> Result<history::Verified<Record>, Error> {
    use crate::history::Kind as _;
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.kind='avatar' AND record.operation_id=$1",
                Record::SELECT
            ),
            &[&operation],
        )
        .await?
        .ok_or(Error::Integrity(
            "an avatar release retains its permanent account advancement",
        ))?;
    history::record(&row, keys, now)
}

pub(crate) async fn corrections(
    client: &Client,
    keys: Keys<'_>,
    ids: &[correction::Id],
    now: DateTime<Utc>,
) -> Result<Vec<history::Verified<Record>>, Error> {
    use crate::history::Kind as _;
    let rows = client
        .query(
            &format!(
                "{} WHERE record.kind='correction' AND record.id=ANY($1)",
                Record::SELECT
            ),
            &[&ids],
        )
        .await?;
    rows.iter()
        .map(|row| history::record(row, keys, now))
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Initialization,
    Registration,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Initialization => "initialization",
            Self::Registration => "registration",
        }
    }

    fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "initialization" => Ok(Self::Initialization),
            "registration" => Ok(Self::Registration),
            _ => Err(Error::Integrity(
                "an account has a declared creation source",
            )),
        }
    }
}

#[cfg_attr(any(test, feature = "test-support"), derive(Clone))]
pub(crate) struct Confirmation {
    pub(super) revision_id: revision::Id,
    pub(super) operator_id: revision::Id,
    pub(super) mobile: Arc<Mobile>,
}

impl Confirmation {
    pub(crate) fn revision_id(&self) -> revision::Id {
        self.revision_id
    }

    pub(crate) fn operator_id(&self) -> revision::Id {
        self.operator_id
    }

    pub(crate) fn mobile(&self) -> &Mobile {
        &self.mobile
    }
}

#[cfg_attr(any(test, feature = "test-support"), derive(Clone))]
pub(crate) enum Event {
    Creation {
        revision_id: revision::Id,
        source: Source,
    },
    Suspension {
        id: suspension::Id,
        confirmation: Confirmation,
        sessions: Vec<session::Id>,
    },
    Resumption {
        id: suspension::Id,
        confirmation: Confirmation,
    },
    Replacement {
        id: transfer::Id,
        operator_id: revision::Id,
        session_id: session::Id,
        message_id: message::Id,
        sessions: Vec<session::Id>,
    },
    Logout {
        initiator_id: session::Id,
        scope: Scope,
        sessions: Vec<session::Id>,
    },
    Correction(correction::Id),
    Avatar {
        id: super::portrait::Id,
        previous: crate::identity::avatar::Revision,
        revision: crate::identity::avatar::Revision,
    },
    Cancellation(cancellation::Id),
}

impl Event {
    fn label(&self) -> &'static str {
        match self {
            Self::Creation { .. } => "creation",
            Self::Suspension { .. } => "suspension",
            Self::Resumption { .. } => "resumption",
            Self::Replacement { .. } => "replacement",
            Self::Logout { .. } => "logout",
            Self::Correction(_) => "correction",
            Self::Avatar { .. } => "avatar",
            Self::Cancellation(_) => "cancellation",
        }
    }
}

pub(crate) struct Record {
    account_id: Id,
    sequence: i64,
    data: Data,
}

#[cfg_attr(any(test, feature = "test-support"), derive(Clone))]
pub(crate) struct Data {
    pub(super) person_id: person::Id,
    pub(super) operation_id: operation::Id,
    pub(super) happened_at: DateTime<Utc>,
    pub(super) event: Event,
}

impl Record {
    pub(crate) fn account_id(&self) -> Id {
        self.account_id
    }

    pub(crate) fn person_id(&self) -> person::Id {
        self.data.person_id
    }

    pub(crate) fn happened_at(&self) -> DateTime<Utc> {
        self.data.happened_at
    }

    pub(crate) fn event(&self) -> &Event {
        &self.data.event
    }

    pub(super) fn matches(&self, state: &State) -> bool {
        if self.account_id != state.id()
            || self.data.person_id != state.person_id()
            || self.sequence != history::State::sequence(state)
            || self.data.happened_at != state.changed_at()
        {
            return false;
        }
        match &self.data.event {
            Event::Creation { .. } => {
                self.sequence == 1
                    && state.created_at() == self.data.happened_at
                    && state
                        .current()
                        .is_some_and(|current| current.suspension_id().is_none())
            }
            Event::Suspension { id, .. } => state
                .current()
                .is_some_and(|current| current.suspension_id() == Some(*id)),
            Event::Resumption { .. } => state
                .current()
                .is_some_and(|current| current.suspension_id().is_none()),
            Event::Cancellation(id) => state.cancellation_id() == Some(*id),
            Event::Replacement { .. } | Event::Logout { .. } | Event::Correction(_) => {
                state.current().is_some()
            }
            Event::Avatar { revision, .. } => state.avatar() == *revision,
        }
    }
}

pub(crate) async fn logout(
    client: &Client,
    keys: Keys<'_>,
    account_id: Id,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<history::Verified<Record>>, Error> {
    use history::Kind as _;
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.kind='logout' AND record.account_id=$1 AND record.operation_id=$2",
                Record::SELECT
            ),
            &[&account_id, &operation_id],
        )
        .await?;
    row.as_ref()
        .map(|row| history::record(row, keys, now))
        .transpose()
}

pub(crate) async fn logouts(
    client: &Client,
    keys: Keys<'_>,
    now: DateTime<Utc>,
) -> Result<Vec<history::Verified<Record>>, Error> {
    records(client, keys, "logout", now).await
}

pub(crate) async fn replacement(
    client: &Client,
    keys: Keys<'_>,
    id: transfer::Id,
    now: DateTime<Utc>,
) -> Result<Option<history::Verified<Record>>, Error> {
    use history::Kind as _;
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.kind='replacement' AND record.id=$1",
                Record::SELECT
            ),
            &[&id],
        )
        .await?;
    row.as_ref()
        .map(|row| history::record(row, keys, now))
        .transpose()
}

pub(crate) async fn replacement_by_operation(
    client: &Client,
    keys: Keys<'_>,
    operation: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<history::Verified<Record>>, Error> {
    use history::Kind as _;
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.kind='replacement' AND record.operation_id=$1",
                Record::SELECT
            ),
            &[&operation],
        )
        .await?;
    row.as_ref()
        .map(|row| history::record(row, keys, now))
        .transpose()
}

pub(crate) async fn replacements(
    client: &Client,
    keys: Keys<'_>,
    now: DateTime<Utc>,
) -> Result<Vec<history::Verified<Record>>, Error> {
    records(client, keys, "replacement", now).await
}

pub(crate) async fn management_records(
    client: &Client,
    keys: Keys<'_>,
    resuming: bool,
    now: DateTime<Utc>,
) -> Result<Vec<history::Verified<Record>>, Error> {
    records(
        client,
        keys,
        if resuming { "resumption" } else { "suspension" },
        now,
    )
    .await
}

async fn records(
    client: &Client,
    keys: Keys<'_>,
    kind: &'static str,
    now: DateTime<Utc>,
) -> Result<Vec<history::Verified<Record>>, Error> {
    use history::{Kind as _, Record as _};
    let mut result = Vec::new();
    let mut cursor: Option<operation::Id> = None;
    loop {
        let sql = format!(
            "{} WHERE record.kind=$2 AND ($1::uuid IS NULL OR record.operation_id>$1) ORDER BY record.operation_id LIMIT 128",
            Record::SELECT,
        );
        let rows = crate::database::verification::Stage::HistoryRecords
            .batch(
                std::time::Duration::from_secs(10),
                client.query(&sql, &[&cursor, &kind]),
            )
            .await??;
        if rows.is_empty() {
            return Ok(result);
        }
        for row in rows {
            let record: history::Verified<Record> = history::record(&row, keys, now)?;
            cursor = Some(record.operation_id());
            result.push(record);
        }
    }
}

pub(crate) async fn management(
    client: &Client,
    keys: Keys<'_>,
    resuming: bool,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<history::Verified<Record>>, Error> {
    use history::Kind as _;
    let kind = if resuming { "resumption" } else { "suspension" };
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.kind=$1 AND record.operation_id=$2",
                Record::SELECT
            ),
            &[&kind, &operation_id],
        )
        .await?;
    row.as_ref()
        .map(|row| history::record(row, keys, now))
        .transpose()
}

pub(crate) struct Suspensions {
    pub(crate) count: i64,
    pub(crate) records: Vec<history::Verified<Record>>,
    pub(crate) resumptions: Vec<history::Verified<Record>>,
}

pub(crate) async fn suspensions(
    client: &Client,
    keys: Keys<'_>,
    account_id: Id,
    pagination: crate::pagination::Pagination,
    now: DateTime<Utc>,
) -> Result<Suspensions, Error> {
    use history::Kind as _;
    let count = client
        .query_one(
            "SELECT count(*) FROM identity.suspensions WHERE account_id=$1",
            &[&account_id],
        )
        .await?
        .try_get(0)
        .map_err(Error::decode)?;
    let rows = client.query(
        &format!("{} WHERE record.kind='suspension' AND record.account_id=$1 ORDER BY record.sequence DESC, record.id DESC LIMIT $2 OFFSET $3", Record::SELECT),
        &[&account_id, &pagination.size(), &pagination.offset()],
    ).await?;
    let mut records = Vec::with_capacity(rows.len());
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows {
        let record: history::Verified<Record> = history::record(&row, keys, now)?;
        let Event::Suspension { id, .. } = record.event() else {
            return Err(Error::Integrity(
                "a suspension page contains suspension records",
            ));
        };
        ids.push(*id);
        records.push(record);
    }
    let rows = client
        .query(
            &format!(
                "{} WHERE record.kind='resumption' AND record.id=ANY($1) ORDER BY record.id",
                Record::SELECT
            ),
            &[&ids],
        )
        .await?;
    let resumptions = rows
        .iter()
        .map(|row| history::record(row, keys, now))
        .collect::<Result<_, _>>()?;
    Ok(Suspensions {
        count,
        records,
        resumptions,
    })
}

/// Recover the permanent creation while later transitions may have advanced the account.
pub(crate) async fn created(
    client: &Client,
    keys: Keys<'_>,
    anchors: &history::Anchors,
    id: Id,
    operation_id: operation::Id,
    source: Source,
    now: DateTime<Utc>,
) -> Result<bool, Error> {
    use history::Kind;

    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.account_id=$1 AND record.sequence=1",
                Record::SELECT
            ),
            &[&id],
        )
        .await?;
    let Some(row) = row else {
        if history::find::<State>(client, keys, anchors, history::Read::Locked, id, now)
            .await?
            .is_some()
        {
            return Err(Error::Integrity(
                "a retained account has its permanent creation record",
            ));
        }
        return Ok(false);
    };
    let record = history::record::<Record>(&row, keys, now)?;
    if record.account_id != id
        || record.data.operation_id != operation_id
        || !matches!(record.data.event, Event::Creation { source: retained, .. } if retained == source)
    {
        return Err(Error::Integrity(
            "account creation retains its original operation and source",
        ));
    }
    let Event::Creation { revision_id, .. } = record.event() else {
        return Err(Error::Integrity("the first account record is its creation"));
    };
    revision::verify_initial(client, record.person_id(), *revision_id).await?;
    anchors.confirm::<State>(&record)?;
    Ok(true)
}

impl history::Kind for Record {
    type Error = Error;
    const NAME: &'static str = "identity.account.record";
    const SELECT: &'static str = r"SELECT record.account_id,record.person_id,record.sequence,record.operation_id,record.happened_at,record.kind,record.id,record.revision_id,record.previous_revision,record.operator_id,record.mobile,record.source,record.session_id,record.message_id,record.sessions,record.scope,record.paired,record.seal,record.seal_key_id,record.seal_version FROM (
        SELECT creation.id AS account_id,creation.person_id,creation.sequence,creation.operation_id,creation.created_at AS happened_at,
            'creation'::text AS kind,creation.id,creation.revision_id,NULL::uuid AS previous_revision,NULL::uuid AS operator_id,NULL::bytea AS mobile,
            creation.source,NULL::uuid AS session_id,NULL::uuid AS message_id,
            NULL::uuid[] AS sessions,NULL::text AS scope,
            true AS paired,
            creation.seal,creation.seal_key_id,creation.seal_version
        FROM identity.creations creation
        UNION ALL
        SELECT suspension.account_id,suspension.person_id,suspension.sequence,suspension.operation_id,suspension.suspended_at,
            'suspension',suspension.id,suspension.revision_id,NULL,suspension.operator_id,suspension.mobile,
            NULL,NULL,NULL,suspension.sessions,NULL,true,suspension.seal,suspension.seal_key_id,suspension.seal_version
        FROM identity.suspensions suspension
        UNION ALL
        SELECT resumption.account_id,resumption.person_id,resumption.sequence,resumption.operation_id,resumption.resumed_at,
            'resumption',resumption.id,resumption.revision_id,NULL,resumption.operator_id,resumption.mobile,
            NULL,NULL,NULL,NULL,NULL,true,resumption.seal,resumption.seal_key_id,resumption.seal_version
        FROM identity.resumptions resumption
        UNION ALL
        SELECT replacement.account_id,replacement.person_id,replacement.sequence,replacement.operation_id,replacement.completed_at,
            'replacement',replacement.id,NULL,NULL,replacement.operator_id,NULL,
            NULL,replacement.session_id,replacement.message_id,replacement.sessions,NULL,
            true,replacement.seal,replacement.seal_key_id,replacement.seal_version
        FROM identity.replacements replacement
        UNION ALL
        SELECT logout.account_id,logout.person_id,logout.sequence,logout.id,logout.revoked_at,
            'logout',logout.id,NULL,NULL,NULL,NULL,NULL,logout.initiator_id,NULL,logout.sessions,logout.scope,
            true,logout.seal,logout.seal_key_id,logout.seal_version
        FROM identity.logouts logout
        UNION ALL
        SELECT portrait.account_id,portrait.person_id,portrait.sequence,portrait.operation_id,portrait.portrait_at,
            'avatar',portrait.id,portrait.revision,portrait.previous_revision,NULL,NULL,NULL,NULL,NULL,NULL,NULL,
            true,portrait.seal,portrait.seal_key_id,portrait.seal_version
        FROM identity.portraits portrait
        UNION ALL
        SELECT projection.account_id,projection.person_id,projection.sequence,projection.operation_id,projection.projected_at,
            projection.cause,projection.id,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,
            CASE projection.cause
                WHEN 'correction' THEN EXISTS (SELECT 1 FROM identity.changes correction
                    WHERE correction.kind='correction' AND correction.id=projection.id AND correction.person_id=projection.person_id
                        AND correction.operation_id=projection.operation_id AND correction.happened_at=projection.projected_at
                        AND correction.transaction_id=projection.transaction_id)
                WHEN 'cancellation' THEN EXISTS (SELECT 1 FROM identity.changes cancellation
                    WHERE cancellation.kind='cancellation' AND cancellation.id=projection.id AND cancellation.account_id=projection.account_id
                        AND cancellation.person_id=projection.person_id AND cancellation.operation_id=projection.operation_id
                        AND cancellation.happened_at=projection.projected_at AND cancellation.transaction_id=projection.transaction_id)
                ELSE false END,
            projection.seal,projection.seal_key_id,projection.seal_version
        FROM identity.projections projection
    ) record";

    fn decode(_: Token, row: &Row, keys: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let sequence: i64 = row.try_get("sequence").map_err(Error::decode)?;
        let happened_at: DateTime<Utc> = row.try_get("happened_at").map_err(Error::decode)?;
        if sequence < 1
            || happened_at > now
            || !row.try_get::<_, bool>("paired").map_err(Error::decode)?
        {
            return Err(Error::Integrity(
                "an account record retains its sequence, time and original projection cause",
            ));
        }
        let event = match row.try_get::<_, &str>("kind").map_err(Error::decode)? {
            "creation" => Event::Creation {
                revision_id: row.try_get("revision_id").map_err(Error::decode)?,
                source: Source::parse(row.try_get("source").map_err(Error::decode)?)?,
            },
            "suspension" => Event::Suspension {
                id: row.try_get("id").map_err(Error::decode)?,
                confirmation: confirmation(row, keys, b"suspension.mobile")?,
                sessions: sessions(row)?,
            },
            "resumption" => Event::Resumption {
                id: row.try_get("id").map_err(Error::decode)?,
                confirmation: confirmation(row, keys, b"resumption.mobile")?,
            },
            "replacement" => Event::Replacement {
                id: row.try_get("id").map_err(Error::decode)?,
                operator_id: row.try_get("operator_id").map_err(Error::decode)?,
                session_id: row.try_get("session_id").map_err(Error::decode)?,
                message_id: row.try_get("message_id").map_err(Error::decode)?,
                sessions: sessions(row)?,
            },
            "logout" => {
                let initiator_id = row.try_get("session_id").map_err(Error::decode)?;
                let scope = row.try_get("scope").map_err(Error::decode)?;
                let sessions = sessions(row)?;
                if !sessions.contains(&initiator_id)
                    || (scope == Scope::Current && sessions.as_slice() != [initiator_id])
                {
                    return Err(Error::Integrity(
                        "a logout retains its initiator and fixed scope",
                    ));
                }
                Event::Logout {
                    initiator_id,
                    scope,
                    sessions,
                }
            }
            "correction" => Event::Correction(row.try_get("id").map_err(Error::decode)?),
            "avatar" => {
                let previous: crate::identity::avatar::Revision =
                    row.try_get("previous_revision").map_err(Error::decode)?;
                let revision: crate::identity::avatar::Revision =
                    row.try_get("revision_id").map_err(Error::decode)?;
                if previous == revision {
                    return Err(Error::Integrity(
                        "an avatar advancement changes its revision",
                    ));
                }
                Event::Avatar {
                    id: row.try_get("id").map_err(Error::decode)?,
                    previous,
                    revision,
                }
            }
            "cancellation" => Event::Cancellation(row.try_get("id").map_err(Error::decode)?),
            _ => {
                return Err(Error::Integrity(
                    "an account record has a declared transition",
                ));
            }
        };
        if matches!(event, Event::Creation { .. }) != (sequence == 1) {
            return Err(Error::Integrity(
                "an account starts with exactly one creation record",
            ));
        }
        Ok(Self {
            account_id: row.try_get("account_id").map_err(Error::decode)?,
            sequence,
            data: Data {
                person_id: row.try_get("person_id").map_err(Error::decode)?,
                operation_id: row.try_get("operation_id").map_err(Error::decode)?,
                happened_at,
                event,
            },
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let mut fields = vec![
            self.account_id.as_bytes().to_vec(),
            self.data.person_id.as_bytes().to_vec(),
            self.sequence.to_be_bytes().to_vec(),
            self.data.operation_id.as_bytes().to_vec(),
            self.data
                .happened_at
                .timestamp_micros()
                .to_be_bytes()
                .to_vec(),
            self.data.event.label().as_bytes().to_vec(),
        ];
        match &self.data.event {
            Event::Creation {
                revision_id,
                source,
            } => {
                fields.extend([
                    revision_id.as_bytes().to_vec(),
                    source.label().as_bytes().to_vec(),
                ]);
            }
            Event::Suspension {
                id,
                confirmation,
                sessions,
            } => {
                fields.push(id.as_bytes().to_vec());
                confirmation.fields(&mut fields);
                fields.extend(sessions.iter().map(|id| id.as_bytes().to_vec()));
            }
            Event::Resumption { id, confirmation } => {
                fields.push(id.as_bytes().to_vec());
                confirmation.fields(&mut fields);
            }
            Event::Replacement {
                id,
                operator_id,
                session_id,
                message_id,
                sessions,
            } => {
                fields.extend([
                    id.as_bytes().to_vec(),
                    operator_id.as_bytes().to_vec(),
                    session_id.as_bytes().to_vec(),
                    message_id.as_bytes().to_vec(),
                ]);
                fields.extend(sessions.iter().map(|id| id.as_bytes().to_vec()));
            }
            Event::Logout {
                initiator_id,
                scope,
                sessions,
            } => {
                fields.extend([
                    initiator_id.as_bytes().to_vec(),
                    scope.as_ref().as_bytes().to_vec(),
                ]);
                fields.extend(sessions.iter().map(|id| id.as_bytes().to_vec()));
            }
            Event::Correction(id) => fields.push(id.as_bytes().to_vec()),
            Event::Avatar {
                id,
                previous,
                revision,
            } => fields.extend([
                id.as_bytes().to_vec(),
                previous.as_bytes().to_vec(),
                revision.as_bytes().to_vec(),
            ]),
            Event::Cancellation(id) => fields.push(id.as_bytes().to_vec()),
        }
        Zeroizing::new(fields)
    }
}

impl Confirmation {
    fn fields(&self, fields: &mut Vec<Vec<u8>>) {
        fields.extend([
            self.revision_id.as_bytes().to_vec(),
            self.operator_id.as_bytes().to_vec(),
            self.mobile.as_bytes().to_vec(),
        ]);
    }
}

fn sessions(row: &Row) -> Result<Vec<session::Id>, Error> {
    let ids: Vec<session::Id> = row.try_get("sessions").map_err(Error::decode)?;
    if ids
        .windows(2)
        .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
    {
        return Err(Error::Integrity(
            "a fixed session set is unique and ordered by identity",
        ));
    }
    Ok(ids)
}

struct Binding {
    id: suspension::Id,
    field: &'static [u8],
}

impl envelope::Binding for Binding {
    fn domain(&self) -> &[u8] {
        b"nucleus/identity/envelope/v1"
    }

    fn identity(&self) -> &[u8] {
        self.id.as_bytes()
    }

    fn field(&self) -> &[u8] {
        self.field
    }
}

struct Plaintext<'a> {
    binding: Binding,
    mobile: &'a Mobile,
}

impl envelope::Binding for Plaintext<'_> {
    fn domain(&self) -> &[u8] {
        self.binding.domain()
    }

    fn identity(&self) -> &[u8] {
        self.binding.identity()
    }

    fn field(&self) -> &[u8] {
        self.binding.field()
    }
}

impl envelope::Input for Plaintext<'_> {
    fn plaintext(&self) -> &[u8] {
        self.mobile.as_bytes()
    }
}

fn confirmation(row: &Row, keys: Keys<'_>, field: &'static [u8]) -> Result<Confirmation, Error> {
    let value: Sealed = row.try_get("mobile").map_err(Error::decode)?;
    let plaintext = keys.data.open(
        &value.0,
        &Binding {
            id: row.try_get("id").map_err(Error::decode)?,
            field,
        },
    )?;
    let mobile = std::str::from_utf8(&plaintext)?
        .parse()
        .map_err(invalid_mobile)?;
    Ok(Confirmation {
        revision_id: row.try_get("revision_id").map_err(Error::decode)?,
        operator_id: row.try_get("operator_id").map_err(Error::decode)?,
        mobile: Arc::new(mobile),
    })
}

fn invalid_mobile(_: crate::identity::mobile::ParseError) -> Error {
    Error::Integrity("an account confirmation retains a valid mobile")
}

/// Move the SMS fixture's account history, retaining the older bootstrap origin.
#[cfg(any(test, feature = "test-support"))]
pub(crate) async fn age(
    client: &Client,
    keys: Keys<'_>,
    now: DateTime<Utc>,
    elapsed: chrono::TimeDelta,
) -> Result<
    (
        Vec<history::Verified<State>>,
        Vec<history::Verified<person::state::State>>,
    ),
    Error,
> {
    use std::collections::HashSet;

    use history::{Kind as _, Seed, State as _};

    let states = history::inspect::<State>(client, keys, now).await?;
    let rows = client.query(Record::SELECT, &[]).await?;
    let originals = rows
        .iter()
        .map(|row| history::record::<Record>(row, keys, now))
        .collect::<Result<Vec<_>, _>>()?;
    let mut registered = HashSet::new();
    let mut persons = Vec::new();
    for record in &originals {
        match record.event() {
            Event::Creation {
                source: Source::Registration,
                ..
            } => {
                registered.insert(record.account_id());
                persons.push(record.person_id());
            }
            Event::Correction(_) | Event::Cancellation(_) => {
                return Err(Error::Integrity(
                    "the SMS age fixture precedes correction and cancellation projections",
                ));
            }
            _ => {}
        }
    }
    let persons = person::record::age(client, keys, now, elapsed, &persons).await?;
    let mut dated_states = Vec::new();
    for state in states {
        let registration = registered.contains(&state.id());
        if !registration && state.sequence() == 1 {
            continue;
        }
        dated_states.push(state.aged(keys, elapsed, registration)?);
    }
    for record in originals {
        let (table, column) = match record.event() {
            Event::Creation {
                source: Source::Initialization,
                ..
            } => continue,
            Event::Creation {
                source: Source::Registration,
                ..
            } => (CREATIONS, "created_at"),
            Event::Suspension { .. } => (SUSPENSIONS, "suspended_at"),
            Event::Resumption { .. } => (RESUMPTIONS, "resumed_at"),
            Event::Replacement { .. } => (REPLACEMENTS, "completed_at"),
            Event::Logout { .. } => (LOGOUTS, "revoked_at"),
            Event::Avatar { .. } => ("identity.portraits", "portrait_at"),
            Event::Correction(_) | Event::Cancellation(_) => {
                return Err(Error::Integrity(
                    "SMS fixture projections were excluded before mutation",
                ));
            }
        };
        let mut data = record.data.clone();
        data.happened_at -= elapsed;
        let dated = Seed::record::<Record>(record.account_id, record.sequence, data, keys)?;
        let account = if table == CREATIONS {
            "id"
        } else {
            "account_id"
        };
        client.execute(
            &format!("UPDATE {table} SET {column}=$3,seal=$4,seal_key_id=$5,seal_version=$6 WHERE {account}=$1 AND sequence=$2"),
            &[&dated.account_id, &dated.sequence, &dated.happened_at(), &dated.seal().tag(),
              &dated.seal().key_id(), &dated.seal().version()],
        ).await?;
    }
    for state in &dated_states {
        super::state::write_aged(client, state).await?;
    }
    Ok((dated_states, persons))
}

impl history::Record for Record {
    const OBJECT: &'static str = "account_id";
    type Object = Id;
    type Data = Data;

    fn build(_: Token, account_id: Id, sequence: i64, data: Data) -> Self {
        Self {
            account_id,
            sequence,
            data,
        }
    }

    fn object(&self) -> Id {
        self.account_id
    }

    fn sequence(&self) -> i64 {
        self.sequence
    }

    fn operation_id(&self) -> operation::Id {
        self.data.operation_id
    }

    fn follows(&self, previous: &Self) -> bool {
        self.account_id == previous.account_id
            && self.data.person_id == previous.data.person_id
            && self.data.happened_at >= previous.data.happened_at
            && !matches!(previous.data.event, Event::Cancellation(_))
    }

    async fn insert(
        &self,
        _: Token,
        client: &Client,
        keys: Keys<'_>,
        seal: &Seal,
    ) -> Result<(), Error> {
        match &self.data.event {
            Event::Creation {
                revision_id,
                source,
            } => {
                client.execute(
                    "INSERT INTO identity.creations (id,person_id,sequence,operation_id,created_at,revision_id,source,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                    &[&self.account_id, &self.data.person_id, &self.sequence, &self.data.operation_id,
                      &self.data.happened_at, revision_id, &source.label(), &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
            }
            Event::Suspension {
                id,
                confirmation,
                sessions,
            } => {
                let mobile = Sealed(keys.data.seal(&Plaintext {
                    binding: Binding {
                        id: *id,
                        field: b"suspension.mobile",
                    },
                    mobile: &confirmation.mobile,
                })?);
                client.execute(
                    "INSERT INTO identity.suspensions (id,account_id,person_id,sequence,operation_id,suspended_at,revision_id,operator_id,mobile,sessions,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
                    &[id, &self.account_id, &self.data.person_id, &self.sequence, &self.data.operation_id,
                      &self.data.happened_at, &confirmation.revision_id, &confirmation.operator_id, &mobile,
                      sessions, &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
            }
            Event::Resumption { id, confirmation } => {
                let mobile = Sealed(keys.data.seal(&Plaintext {
                    binding: Binding {
                        id: *id,
                        field: b"resumption.mobile",
                    },
                    mobile: &confirmation.mobile,
                })?);
                client.execute(
                    "INSERT INTO identity.resumptions (id,account_id,person_id,sequence,operation_id,resumed_at,revision_id,operator_id,mobile,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
                    &[id, &self.account_id, &self.data.person_id, &self.sequence, &self.data.operation_id,
                      &self.data.happened_at, &confirmation.revision_id, &confirmation.operator_id, &mobile,
                      &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
            }
            Event::Replacement {
                id,
                operator_id,
                session_id,
                message_id,
                sessions,
            } => {
                client.execute(
                    "INSERT INTO identity.replacements (id,account_id,person_id,sequence,operation_id,completed_at,operator_id,session_id,message_id,sessions,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
                    &[id, &self.account_id, &self.data.person_id, &self.sequence, &self.data.operation_id,
                      &self.data.happened_at, operator_id, session_id, message_id,
                      sessions, &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
            }
            Event::Logout {
                initiator_id,
                scope,
                sessions,
            } => {
                client.execute(
                    "INSERT INTO identity.logouts (id,account_id,person_id,sequence,revoked_at,initiator_id,scope,sessions,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
                    &[&self.data.operation_id, &self.account_id, &self.data.person_id, &self.sequence,
                      &self.data.happened_at, initiator_id, scope, sessions, &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
            }
            Event::Correction(id) => {
                client.execute(
                    "INSERT INTO identity.projections (id,cause,account_id,person_id,sequence,operation_id,projected_at,seal,seal_key_id,seal_version) VALUES ($1,'correction',$2,$3,$4,$5,$6,$7,$8,$9)",
                    &[id, &self.account_id, &self.data.person_id, &self.sequence, &self.data.operation_id,
                      &self.data.happened_at, &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
            }
            Event::Avatar {
                id,
                previous,
                revision,
            } => {
                client.execute(
                    "INSERT INTO identity.portraits (id,account_id,person_id,sequence,operation_id,portrait_at,previous_revision,revision,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
                    &[id, &self.account_id, &self.data.person_id, &self.sequence, &self.data.operation_id,
                      &self.data.happened_at, previous, revision, &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
            }
            Event::Cancellation(id) => {
                client.execute(
                    "INSERT INTO identity.projections (id,cause,account_id,person_id,sequence,operation_id,projected_at,seal,seal_key_id,seal_version) VALUES ($1,'cancellation',$2,$3,$4,$5,$6,$7,$8,$9)",
                    &[id, &self.account_id, &self.data.person_id, &self.sequence, &self.data.operation_id,
                      &self.data.happened_at, &seal.tag(), &seal.key_id(), &seal.version()],
                ).await?;
            }
        }
        Ok(())
    }
}
