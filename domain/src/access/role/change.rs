//! Permanent role definition history, independent of the current role.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::{
    Client, Row,
    types::{FromSql, ToSql},
};
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    access::{
        capability::Scope,
        role::{self, Code, Error, Kind, Level, Name, Reader, Role, state},
    },
    history::{self, Kind as HistoryKind, State as _, Token, Verified},
    identity::{operation, person::revision},
    keys::seal::Seal,
    pagination::Pagination,
};

pub(super) const TABLE: &str = "access.changes";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromSql, ToSql)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = role::change::Id, value_type = Uuid)]
pub struct Id(Uuid);

impl Id {
    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Filter {
    #[serde(flatten)]
    pagination: Pagination,
    role_id: Option<role::Id>,
    operation_id: Option<operation::Id>,
}

impl Filter {
    pub fn new(
        pagination: Pagination,
        role_id: Option<role::Id>,
        operation_id: Option<operation::Id>,
    ) -> Self {
        Self {
            pagination,
            role_id,
            operation_id,
        }
    }
}

/// Delete has no permission definition; it does not represent an empty permission set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
#[schema(as = role::change::Action)]
pub enum Action {
    Create { permissions: Vec<Code> },
    Update { permissions: Vec<Code> },
    Delete,
}

pub struct Change {
    id: Id,
    role_id: role::Id,
    action: Action,
    name: Name,
    level: Level,
    operator: revision::Identity,
    changed_at: DateTime<Utc>,
    operation_id: operation::Id,
}

impl Change {
    pub(super) fn deleted(
        id: Id,
        role: &Role,
        operator: revision::Identity,
        changed_at: DateTime<Utc>,
        operation_id: operation::Id,
    ) -> Self {
        Self {
            id,
            role_id: role.id(),
            action: Action::Delete,
            name: role.name().clone(),
            level: role.level(),
            operator,
            changed_at,
            operation_id,
        }
    }
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn role_id(&self) -> role::Id {
        self.role_id
    }
    pub fn action(&self) -> &Action {
        &self.action
    }
    pub fn name(&self) -> &Name {
        &self.name
    }
    pub fn level(&self) -> Level {
        self.level
    }
    pub fn operator(&self) -> &revision::Identity {
        &self.operator
    }
    pub fn changed_at(&self) -> DateTime<Utc> {
        self.changed_at
    }
    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
}

pub struct List {
    count: i64,
    changes: Vec<Change>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }
}

pub(in crate::access) struct Record {
    id: Id,
    role_id: role::Id,
    previous_id: Option<Id>,
    sequence: i64,
    scope: Scope,
    entry: Option<crate::access::Entry>,
    action: Action,
    name: Name,
    level: Level,
    operator_id: revision::Id,
    changed_at: DateTime<Utc>,
    operation_id: operation::Id,
}

pub(in crate::access) struct Data {
    pub(super) id: Id,
    pub(super) previous_id: Option<Id>,
    pub(super) scope: Scope,
    pub(super) entry: Option<crate::access::Entry>,
    pub(super) action: Action,
    pub(super) name: Name,
    pub(super) level: Level,
    pub(super) operator_id: revision::Id,
    pub(super) changed_at: DateTime<Utc>,
    pub(super) operation_id: operation::Id,
}

impl HistoryKind for Record {
    type Error = history::Error;
    const NAME: &'static str = "access.change";
    const SELECT: &'static str = "SELECT record.id,record.role_id,record.previous_id,record.sequence,record.scope,record.entry,record.action,record.name,record.level,record.permissions,record.operator_id,record.changed_at,record.operation_id,record.seal,record.seal_key_id,record.seal_version FROM access.changes record";

    fn decode(
        _: Token,
        row: &Row,
        _: history::Keys<'_>,
        now: DateTime<Utc>,
    ) -> Result<Self, history::Error> {
        let scope = row.try_get("scope").map_err(history::Error::decode)?;
        let name = row.try_get("name").map_err(history::Error::decode)?;
        let level: Level = row.try_get("level").map_err(history::Error::decode)?;
        let option: Option<crate::access::Entry> =
            row.try_get("entry").map_err(history::Error::decode)?;
        if (scope == Scope::Workforce && (2..=999).contains(&level.0)) != option.is_some() {
            return Err(history::Error::Integrity(
                "custom Workforce history retains its explicit authority entry",
            ));
        }
        let permissions: Option<Vec<Code>> =
            row.try_get("permissions").map_err(history::Error::decode)?;
        if let Some(permissions) = &permissions {
            Kind::new(scope, &name, level, permissions).map_err(|_| {
                history::Error::Integrity(
                    "a recorded definition retains its scoped permissions and system kind",
                )
            })?;
            if permissions
                .windows(2)
                .any(|pair| pair[0].as_ref() >= pair[1].as_ref())
            {
                return Err(history::Error::Integrity(
                    "recorded permissions have canonical code order",
                ));
            }
        }
        let previous_id: Option<Id> = row.try_get("previous_id").map_err(history::Error::decode)?;
        let sequence: i64 = row.try_get("sequence").map_err(history::Error::decode)?;
        let changed_at = row.try_get("changed_at").map_err(history::Error::decode)?;
        if changed_at > now || sequence < 1 || (sequence == 1) != previous_id.is_none() {
            return Err(history::Error::Integrity(
                "role changes retain a non-future time, sequence and explicit predecessor",
            ));
        }
        let action = match (
            row.try_get::<_, &str>("action")
                .map_err(history::Error::decode)?,
            permissions,
        ) {
            ("create", Some(permissions)) if sequence == 1 => Action::Create { permissions },
            ("update", Some(permissions)) if sequence > 1 && (2..=999).contains(&level.0) => {
                Action::Update { permissions }
            }
            ("delete", None) if sequence > 1 && (2..=999).contains(&level.0) => Action::Delete,
            _ => {
                return Err(history::Error::Integrity(
                    "history action retains its complete definition or a custom-role deletion",
                ));
            }
        };
        Ok(Self {
            id: row.try_get("id").map_err(history::Error::decode)?,
            role_id: row.try_get("role_id").map_err(history::Error::decode)?,
            previous_id,
            sequence,
            scope,
            entry: option,
            action,
            name,
            level,
            operator_id: row.try_get("operator_id").map_err(history::Error::decode)?,
            changed_at,
            operation_id: row
                .try_get("operation_id")
                .map_err(history::Error::decode)?,
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let mut fields = vec![
            self.id.as_bytes().to_vec(),
            self.role_id.as_bytes().to_vec(),
            self.previous_id
                .map(|id| id.as_bytes().to_vec())
                .unwrap_or_default(),
            self.sequence.to_string().into_bytes(),
            self.scope.as_ref().as_bytes().to_vec(),
            self.entry
                .map(|entry| entry.as_ref().as_bytes().to_vec())
                .unwrap_or_default(),
            self.action_name().as_bytes().to_vec(),
            self.name.0.as_bytes().to_vec(),
            self.level.0.to_string().into_bytes(),
            self.operator_id.as_bytes().to_vec(),
            self.changed_at.timestamp_micros().to_string().into_bytes(),
            self.operation_id.as_bytes().to_vec(),
        ];
        if let Some(permissions) = self.permissions() {
            fields.push(permissions.len().to_string().into_bytes());
            fields.extend(
                permissions
                    .iter()
                    .map(|code| code.as_ref().as_bytes().to_vec()),
            );
        } else {
            fields.push(Vec::new());
        }
        Zeroizing::new(fields)
    }
}

impl history::Record for Record {
    const OBJECT: &'static str = "role_id";
    type Object = role::Id;
    type Data = Data;

    fn build(_: Token, role_id: role::Id, sequence: i64, data: Data) -> Self {
        Self {
            id: data.id,
            role_id,
            previous_id: data.previous_id,
            sequence,
            scope: data.scope,
            entry: data.entry,
            action: data.action,
            name: data.name,
            level: data.level,
            operator_id: data.operator_id,
            changed_at: data.changed_at,
            operation_id: data.operation_id,
        }
    }
    fn object(&self) -> role::Id {
        self.role_id
    }
    fn sequence(&self) -> i64 {
        self.sequence
    }
    fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
    fn follows(&self, previous: &Self) -> bool {
        self.previous_id == Some(previous.id) && self.scope == previous.scope
    }

    async fn insert(
        &self,
        _: Token,
        client: &Client,
        _: history::Keys<'_>,
        seal: &Seal,
    ) -> Result<(), history::Error> {
        let permissions = self
            .permissions()
            .map(|values| values.iter().map(AsRef::as_ref).collect::<Vec<&str>>());
        client.execute("INSERT INTO access.changes (id,role_id,previous_id,sequence,scope,entry,action,name,level,permissions,operator_id,changed_at,operation_id,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)", &[&self.id,&self.role_id,&self.previous_id,&self.sequence,&self.scope.as_ref(),&self.entry,&self.action_name(),&self.name,&self.level,&permissions,&self.operator_id,&self.changed_at,&self.operation_id,&seal.tag(),&seal.key_id(),&seal.version()]).await?;
        Ok(())
    }
}

impl Record {
    pub(super) fn id(&self) -> Id {
        self.id
    }
    pub(super) fn initialization(
        &self,
        operation_id: operation::Id,
        revision_id: revision::Id,
        at: DateTime<Utc>,
    ) -> Option<super::Builtin> {
        if self.operation_id != operation_id
            || self.operator_id != revision_id
            || self.changed_at != at
            || self.sequence != 1
            || self.previous_id.is_some()
            || self.entry.is_some()
            || !matches!(self.action, Action::Create { .. })
        {
            return None;
        }
        super::Builtin::ALL
            .into_iter()
            .find(|builtin| builtin.scope() == self.scope && builtin.level() == self.level.0)
    }

    pub(super) fn operator_id(&self) -> revision::Id {
        self.operator_id
    }

    pub(super) fn completed_by(&self, completion: &crate::kernel::completion::Record) -> bool {
        completion.operation_id() == self.operation_id
            && completion.action() == self.action_name()
            && completion.revision_id() == self.operator_id
            && completion.created_at() == self.changed_at
            && completion.target()
                == if self.action_name() == "create" {
                    "roles".to_owned()
                } else {
                    self.role_id.0.to_string()
                }
    }

    pub(super) fn agrees(&self, action: &super::operation::Action) -> bool {
        self.scope == action.scope()
            && self.entry == action.entry()
            && self.action_name() == action.name()
            && self.previous_id == action.previous()
            && action.id().is_none_or(|id| id == self.role_id)
            && match action.definition() {
                Some(definition) => {
                    self.name == *definition.name
                        && self.level == definition.level
                        && self.permissions() == Some(definition.permissions)
                }
                None => self.permissions().is_none(),
            }
    }

    fn action_name(&self) -> &'static str {
        match self.action {
            Action::Create { .. } => "create",
            Action::Update { .. } => "update",
            Action::Delete => "delete",
        }
    }

    pub(super) fn permissions(&self) -> Option<&[Code]> {
        match &self.action {
            Action::Create { permissions } | Action::Update { permissions } => Some(permissions),
            Action::Delete => None,
        }
    }

    pub(super) fn matches(
        &self,
        role: &Role,
        scope: Scope,
        active: bool,
        changed_at: DateTime<Utc>,
    ) -> bool {
        self.id == role.change_id
            && self.role_id == role.id
            && self.scope == scope
            && self.name == role.name
            && self.level == role.level
            && self.changed_at == changed_at
            && match self.permissions() {
                Some(permissions) => active && permissions == role.permissions,
                None => !active && role.permissions.is_empty(),
            }
    }

    pub(super) fn definition(&self) -> Result<Role, Error> {
        let permissions = self
            .permissions()
            .ok_or(Error::Facts {
                rule: "a definition snapshot precedes deletion",
            })?
            .to_vec();
        Ok(Role {
            id: self.role_id,
            name: self.name.clone(),
            level: self.level,
            kind: Kind::new(self.scope, &self.name, self.level, &permissions)?,
            permissions,
            change_id: self.id,
        })
    }

    fn to_change(&self, operator: revision::Identity) -> Change {
        Change {
            id: self.id,
            role_id: self.role_id,
            action: self.action.clone(),
            name: self.name.clone(),
            level: self.level,
            operator,
            changed_at: self.changed_at,
            operation_id: self.operation_id,
        }
    }

    pub(super) async fn deliver(
        &self,
        client: &Client,
        revisions: &revision::Reader,
    ) -> Result<Change, Error> {
        let operator = revisions.fetch_identity(client, self.operator_id).await?;
        Ok(self.to_change(operator))
    }
}

pub(super) const COLUMNS: &str = "record.id,record.role_id,record.previous_id,record.sequence,record.scope,record.entry,record.action,record.name,record.level,record.permissions,record.operator_id,record.changed_at,record.operation_id,record.seal,record.seal_key_id,record.seal_version";

pub(super) async fn original(
    client: &Client,
    roles: &Reader,
    operation_id: operation::Id,
    scope: Scope,
    now: DateTime<Utc>,
) -> Result<Option<Verified<Record>>, Error> {
    let row = client.query_opt(
        &format!("{} WHERE record.operation_id=$1 AND record.scope=$2 AND record.level BETWEEN 2 AND 999", Record::SELECT),
        &[&operation_id, &scope.as_ref()],
    ).await?;
    row.as_ref()
        .map(|row| history::record(row, roles.app.keys(), now).map_err(Error::from))
        .transpose()
}

pub(super) async fn fetch_list(
    client: &Client,
    roles: &Reader,
    filter: &Filter,
    scope: Scope,
    now: DateTime<Utc>,
    revisions: &revision::Reader,
) -> Result<List, Error> {
    let states = roles.all(client, now).await?;
    let expected = states
        .iter()
        .filter(|state| state.scope() == scope)
        .try_fold(0_i64, |sum, state| {
            sum.checked_add(state.sequence()).ok_or(Error::Facts {
                rule: "role history count is representable",
            })
        })?;
    let counts = client.query_one(
        "SELECT count(*) AS total, count(*) FILTER (WHERE ($1::uuid IS NULL OR role_id=$1) AND ($2::uuid IS NULL OR operation_id=$2)) AS selected FROM access.changes WHERE scope=$3",
        &[&filter.role_id, &filter.operation_id, &scope.as_ref()],
    ).await?;
    if counts.try_get::<_, i64>("total").map_err(Error::decode)? != expected {
        return Err(Error::Facts {
            rule: "retained role states account for the complete history",
        });
    }
    let count = counts.try_get("selected").map_err(Error::decode)?;
    let statement = format!(
        "{} WHERE record.scope=$5 AND ($1::uuid IS NULL OR record.role_id=$1) AND ($2::uuid IS NULL OR record.operation_id=$2) ORDER BY record.changed_at DESC, record.id DESC LIMIT $3 OFFSET $4",
        Record::SELECT
    );
    let rows = client
        .query(
            &statement,
            &[
                &filter.role_id,
                &filter.operation_id,
                &filter.pagination.size(),
                &filter.pagination.offset(),
                &scope.as_ref(),
            ],
        )
        .await?;
    let mut page = Vec::with_capacity(rows.len());
    let mut ids = Vec::with_capacity(rows.len());
    let mut operators = Vec::with_capacity(rows.len());
    for row in &rows {
        let record = history::record::<Record>(row, roles.app.keys(), now)?;
        if !ids.contains(&record.role_id) {
            ids.push(record.role_id);
        }
        if !operators.contains(&record.operator_id) {
            operators.push(record.operator_id);
        }
        page.push(record);
    }
    // Even an empty or out-of-range filtered page must retain the selected object's history.
    if let Some(id) = filter.role_id
        && states
            .iter()
            .any(|state| state.id() == id && state.scope() == scope)
        && !ids.contains(&id)
    {
        ids.push(id);
    }
    verify_states(client, roles, &ids, scope, now, &states).await?;
    let identities = revisions.fetch_identities(client, &operators).await?;
    let mut changes = Vec::with_capacity(page.len());
    for record in page {
        let operator = identities
            .get(&record.operator_id)
            .ok_or(revision::ReadError::Missing)?
            .clone();
        changes.push(record.to_change(operator));
    }
    Ok(List { count, changes })
}

async fn verify_states(
    client: &Client,
    roles: &Reader,
    ids: &[role::Id],
    scope: Scope,
    now: DateTime<Utc>,
    states: &[Verified<state::State>],
) -> Result<(), Error> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut progress = HashMap::new();
    for id in ids {
        let state = states
            .iter()
            .find(|state| state.id() == *id && state.scope() == scope)
            .ok_or(Error::Facts {
                rule: "historical roles retain their scoped state, including deletion",
            })?;
        progress.insert(*id.as_bytes(), (state, 0_i64, None));
    }
    let statement = format!(
        "{} WHERE record.role_id=ANY($1) AND ($2::uuid IS NULL OR (record.role_id,record.sequence)>($2,$3)) ORDER BY record.role_id,record.sequence LIMIT 128",
        Record::SELECT
    );
    let mut cursor: Option<role::Id> = None;
    let mut position = 0_i64;
    loop {
        let rows = client
            .query(&statement, &[&ids, &cursor, &position])
            .await?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let record = history::record::<Record>(row, roles.app.keys(), now)?;
            let (state, sequence, previous) =
                progress
                    .get_mut(record.role_id.as_bytes())
                    .ok_or(Error::Facts {
                        rule: "history query stays within selected objects",
                    })?;
            *sequence = sequence.checked_add(1).ok_or(Error::Facts {
                rule: "role history sequence is representable",
            })?;
            if record.scope != scope
                || record.sequence != *sequence
                || record.previous_id != *previous
                || record.sequence > state.sequence()
                || (record.sequence == state.sequence() && !state.has_head(&record))
            {
                return Err(Error::Facts {
                    rule: "role records are contiguous, linked and end at the sealed state",
                });
            }
            *previous = Some(record.id);
            cursor = Some(record.role_id);
            position = record.sequence;
        }
    }
    if progress
        .values()
        .any(|(state, sequence, _)| state.sequence() != *sequence)
    {
        return Err(Error::Facts {
            rule: "selected role history reaches its retained head",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use chrono::DateTime;
    use platform::cryptography::sha256;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use crate::{
        access::{
            capability::Scope,
            role::{
                self, Error,
                change::{Action, Data, Filter, Id, Record},
                tests::{actor, assign_role, insert_role, seed_history, start},
            },
        },
        history::{self, Kind as _, Seed},
        identity::{
            authentication::{
                self, Database, Supplier, server,
                session::{
                    Receipt,
                    tests::{CODE, input, message},
                },
            },
            current::Proof,
            person::revision,
        },
        kernel,
        keys::{self, Derived, Entry, Material, seal},
        pagination::Pagination,
    };

    #[test]
    fn role_change_encoding_matches_independent_python_vectors() {
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
        let keys = history::Keys {
            data: &inventory.encryption,
            seals: &seals,
        };
        let previous =
            Id(Uuid::parse_str("00000000-0000-4000-8000-000000000003").expect("previous"));
        // Python struct.pack(">I"), hashlib.sha256 and hmac; NULL permissions differ from a zero count.
        for (
            sequence,
            scope,
            entry,
            previous_id,
            action,
            name,
            level,
            count,
            length,
            digest,
            tag,
        ) in [
            (
                1,
                Scope::Institution,
                None,
                None,
                Action::Create {
                    permissions: Vec::new(),
                },
                "审阅 Café",
                237,
                13,
                207,
                "9aca8dae58ffbc4be5298b963ae1854651444239b492f97657cea3cc549adde4",
                "60ee4dcbd7f73e521c458e82fa60573eef609439fa6371f88738bff5723f03a9",
            ),
            (
                12,
                Scope::Workforce,
                Some(crate::access::Entry::Institution),
                Some(previous),
                Action::Update {
                    permissions: vec![role::Code::WorkforceDirectory, role::Code::WorkforceNumbers],
                },
                "值班 Café",
                456,
                15,
                277,
                "ec55fbd9241e4847fdb822039c8ddfb9dfd27a0439db46a795107af1373acabe",
                "4d3dfe63facb049e265eb2d7c4f028cf5919769bc5df61a8c43b6efd54347085",
            ),
            (
                13,
                Scope::Workforce,
                Some(crate::access::Entry::Domain),
                Some(previous),
                Action::Delete,
                "值班 Café",
                456,
                13,
                227,
                "ae7b22016faeb8f03750fcf073bab8fe9cc8d09163063a94d736bdeaf30c8bd2",
                "ccbe811b8ecbf38da53c7b09174193d149268c8165a9567869f9807098b92219",
            ),
        ] {
            let record =
                Seed::record::<Record>(
                    "00000000-0000-4000-8000-000000000001"
                        .parse()
                        .expect("role"),
                    sequence,
                    Data {
                        id: Id(Uuid::parse_str("00000000-0000-4000-8000-000000000002")
                            .expect("record")),
                        previous_id,
                        scope,
                        entry,
                        action,
                        name: name.parse().expect("NFC name"),
                        level: role::Level(level),
                        operator_id: serde_json::from_str(
                            r#""00000000-0000-4000-8000-000000000004""#,
                        )
                        .expect("operator revision"),
                        changed_at: DateTime::from_timestamp_micros(1_700_000_000_123_456)
                            .expect("microseconds"),
                        operation_id: "00000000-0000-4000-8000-000000000005"
                            .parse()
                            .expect("operation"),
                    },
                    keys,
                )
                .expect("seed record");
            let fields = record.fields();
            assert_eq!(fields.len(), count);
            if sequence == 1 {
                assert_eq!(fields[12], b"0");
            }
            if sequence == 13 {
                assert!(fields[12].is_empty());
            }
            let encoded = seal::encode_fields(
                Record::NAME,
                Record::VERSION,
                &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            )
            .expect("encoding");
            assert_eq!(encoded.as_ref().len(), length);
            assert_eq!(hex::encode(sha256::Digest::new(&encoded).as_ref()), digest);
            assert_eq!(hex::encode(record.seal().tag()), tag);
        }
    }

    fn filter() -> Filter {
        Filter::new(Pagination::new(1, 100).expect("page"), None, None)
    }

    #[test]
    fn history_batches_full_pages_without_per_change_round_trips() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let proxy = Proxy::start(&server).await;
            let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
            let manager = actor(&database, &services, &mut supplier).await;
            timeout(Duration::from_secs(60), Box::pin(async {
                for index in 0..98 {
                    Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(serde_json::json!({"name":format!("History role {index}"),"level":10,"permissions":[]})).expect("definition"))).await.expect("real custom role");
                }
            })).await.expect("bounded role preparation");
            timeout(Duration::from_secs(60), Box::pin(async {
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let rows = observer.client().query("SELECT id, role_id, operator_id, operation_id FROM access.changes WHERE scope='institution' ORDER BY changed_at DESC, id DESC", &[]).await.expect("committed full page before reading");
                assert_eq!(rows.len(), 100);
                let mut baseline = None;
                for size in [1, 100] {
                    proxy.arm(Fault::DelayedStateQueries);
                    let started = std::time::Instant::now();
                    let result = services.role().changes(&manager, Filter::new(Pagination::new(1, size).expect("page"), None, None)).await;
                    let queries = proxy.delayed_queries();
                    eprintln!("Role change history size={size}: {:?}; queries delayed by 8 ms: {queries}", started.elapsed());
                    let page = result.expect("complete history within the existing read deadline");
                    assert!(queries > 0, "the delayed history transaction was observed");
                    assert_eq!(*baseline.get_or_insert(queries), queries, "a full page uses the same queries as a single row");
                    assert_eq!(page.count(), 100);
                    assert_eq!(page.changes().len(), usize::try_from(size).expect("page size"));
                    for (change, row) in page.changes().iter().zip(&rows) {
                        assert_eq!(change.id(), row.get("id"));
                        assert_eq!(change.role_id(), row.get("role_id"));
                        assert_eq!(change.operation_id(), row.get("operation_id"));
                        assert_eq!(change.operator().revision_id(), row.get::<_, revision::Id>("operator_id"));
                    }
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE scope='institution'", &[]).await.expect("reading preserves facts").get::<_, i64>(0), 100);
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded full history acceptance");
        }));
    }

    #[test]
    fn history_reads_original_definitions_and_operators_after_current_state_changes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                let facts = owner.client().query("SELECT id,role_id,operator_id,operation_id FROM access.changes WHERE scope='institution' ORDER BY changed_at DESC,id DESC", &[]).await.expect("independent original history");
                let page = services.role().changes(&administrator, filter()).await.expect("system history");
                assert_eq!(page.count(), 2);
                for (change, fact) in page.changes().iter().zip(&facts) {
                    assert_eq!(change.id(), fact.get("id")); assert_eq!(change.role_id(), fact.get("role_id"));
                    assert_eq!(change.operator().revision_id(), fact.get("operator_id")); assert_eq!(change.operator().name(), "隔*");
                    assert!(matches!(change.action(), Action::Create { .. }));
                }
                let operation = facts[0].get("operation_id");
                let original = facts[0].get("role_id");
                for (role_id, operation_id, expected) in [(None, Some(operation), 2), (Some(original), None, 1), (Some(original), Some(operation), 1), (Some(role::Id::allocate()), Some(operation), 0)] {
                    let page = services.role().changes(&administrator, Filter::new(Pagination::new(1,20).expect("page"),role_id,operation_id)).await.expect("independent and combined filters");
                    assert_eq!(page.count(), expected); assert_eq!(i64::try_from(page.changes().len()).expect("length"),expected);
                }
                let page = services.role().changes(&administrator, Filter::new(Pagination::new(i64::MAX,1).expect("last page"),None,None)).await.expect("out of range page");
                assert_eq!(page.count(),2); assert!(page.changes().is_empty());
                let id = message(&database, services.authentication(), &mut supplier, "13800138000").await;
                let Receipt::Registration(credential) = Box::pin(services.authentication().create_session(input(Uuid::new_v4(), id, "13800138000", CODE))).await.expect("R1") else { panic!("registration"); };
                let issued = Box::pin(services.registration().register_input_for_test(crate::registration::Input::new(Uuid::new_v4().to_string().parse().expect("operation"),credential.id(),credential.token().text().parse().expect("token"),"Reader".parse().expect("name"),"00000020000229007X".parse().expect("number")))).await.expect("R2");
                let reader = services.current().authenticate(Proof::Cookie { id:issued.id(),token:issued.token().text().parse().expect("token") }).await.expect("reader actor");
                assert!(matches!(services.role().changes(&reader, filter()).await,Err(kernel::Error::Denied)));
                let assignments = insert_role(owner.client(), services.role(), &administrator, "Assignments", 200, &["institution.assignments"]).await;
                assign_role(owner.client(), services.role(), &reader, assignments).await;
                assert!(matches!(services.role().changes(&reader, filter()).await,Err(kernel::Error::Denied)));
                let allowed = insert_role(owner.client(), services.role(), &administrator, "Reader", 300, &["institution.roles"]).await;
                assign_role(owner.client(), services.role(), &reader, allowed).await;
                let past = role::Id::allocate();
                let operator_id = services.current().read(&administrator).await.expect("original operator").revision_id();
                let created_at = context.sample().await.expect("fixture time") - chrono::TimeDelta::seconds(3);
                let records = [("Before", Action::Create { permissions: Vec::new() }), ("After", Action::Update { permissions: Vec::new() }), ("After", Action::Delete)]
                    .into_iter().enumerate().map(|(index, (name, action))| Data {
                        id: super::Id::allocate(), previous_id: None, scope: Scope::Institution,
                        entry: None, action, name: name.parse().expect("fixture name"),
                        level: 100.try_into().expect("fixture level"), operator_id,
                        changed_at: created_at + chrono::TimeDelta::seconds(i64::try_from(index).expect("fixture index")),
                        operation_id: Uuid::new_v4().to_string().parse().expect("fixture operation"),
                    }).collect();
                seed_history(owner.client(), services.role(), past, records).await;
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("future management fixture");
                let person_id = page_person(owner.client(), administrator.session_id()).await;
                let revision_id = revision::Id::allocate();
                revision::fixture(owner.client(),services.role().app.keys(),revision::FixtureSource::Person,revision_id,person_id,&"Later Operator".parse().expect("name"),&"000000200002290010".parse().expect("number")).await.expect("changed operator identity");
                owner.client().execute("UPDATE identity.persons SET revision_id=$1 WHERE id=$2", &[&revision_id,&person_id]).await.expect("current version");
                owner.client().execute("DELETE FROM identity.handoffs WHERE id IN(SELECT id FROM identity.sessions WHERE account_id=(SELECT id FROM identity.accounts WHERE person_id=$1))", &[&person_id]).await.expect("end original credentials");
                owner.client().execute("DELETE FROM identity.sessions WHERE account_id=(SELECT id FROM identity.accounts WHERE person_id=$1)", &[&person_id]).await.expect("end original sessions");
                owner.client().execute("DELETE FROM access.holders WHERE person_id=$1", &[&person_id]).await.expect("remove current holder");
                owner.client().execute("DELETE FROM identity.avatars WHERE account_id=(SELECT id FROM identity.accounts WHERE person_id=$1)", &[&person_id]).await.expect("remove account image");
                owner.client().execute("DELETE FROM identity.accounts WHERE person_id=$1", &[&person_id]).await.expect("original account removed");
                owner.client().batch_execute("COMMIT").await.expect("isolated later lifecycle");
                let history = services.role().changes(&reader, Filter::new(Pagination::new(1,20).expect("page"),Some(past),None)).await.expect("permanent history after deletion");
                assert_eq!(history.count(),3); assert!(matches!(history.changes()[0].action(),Action::Delete));
                assert!(matches!(history.changes()[1].action(),Action::Update { permissions } if permissions.is_empty()));
                assert_eq!(serde_json::to_value(history.changes()[0].action()).expect("delete protocol"),serde_json::json!({"action":"delete"}));
                assert_eq!(serde_json::to_value(history.changes()[1].action()).expect("empty update protocol"),serde_json::json!({"action":"update","permissions":[]}));
                for change in history.changes() { assert_eq!(change.operator().person_id(),person_id); assert_eq!(change.operator().name(),"隔*"); assert_ne!(change.operator().revision_id(),revision_id); }
                owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            }).await.expect("history lifecycle budget");
        });
    }

    async fn page_person(
        client: &tokio_postgres::Client,
        id: crate::identity::session::Id,
    ) -> crate::identity::person::Id {
        client.query_one("SELECT person_id FROM identity.accounts WHERE id=(SELECT account_id FROM identity.sessions WHERE id=$1)",&[&id]).await.expect("original person").get(0)
    }

    #[test]
    fn filtered_history_validates_later_records_across_batch_boundaries() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15),async {
                let database=Database::initialized(server).await; let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let actor=actor(&database,&services,&mut supplier).await;
                let owner=database.connections("postgres").recover().await.expect("owner");
                let role=role::Id::allocate();
                let operation=Uuid::new_v4().to_string().parse().expect("original operation");
                let operator_id=services.current().read(&actor).await.expect("fixture operator").revision_id();
                let created_at=context.sample().await.expect("fixture time")-chrono::TimeDelta::seconds(200);
                let records=(0..=130).map(|index| Data {
                    id:super::Id::allocate(),previous_id:None,scope:Scope::Institution,entry:None,
                    action:if index==0 {Action::Create {permissions:Vec::new()}} else {Action::Update {permissions:Vec::new()}},
                    name:format!("Version {index}").parse().expect("fixture name"),
                    level:200.try_into().expect("fixture level"),operator_id,
                    changed_at:created_at+chrono::TimeDelta::seconds(index),
                    operation_id:if index==0 {operation} else {Uuid::new_v4().to_string().parse().expect("successor operation")},
                }).collect();
                seed_history(owner.client(),services.role(),role,records).await;
                let selected=||Filter::new(Pagination::new(1,1).expect("one result"),Some(role),Some(operation));
                assert_eq!(services.role().changes(&actor,selected()).await.expect("valid long history").count(),1);
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("late corruption");
                owner.client().execute("UPDATE access.changes SET action='create' WHERE role_id=$1 AND name='Version 129'",&[&role]).await.expect("invalid late recreation outside filtered page");
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                assert!(matches!(services.role().changes(&actor,selected()).await,Err(error) if error.is_corrupt()));
                assert!(services.ready().await.is_err(), "historical damage outside the page closes shared admission");
                owner.terminate().await.expect("ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            }).await.expect("bounded historical scan");
        });
    }

    #[test]
    fn history_authenticates_both_original_identity_fields_before_disclosing_a_masked_operator() {
        use platform::cryptography::envelope::Envelope;
        use zeroize::Zeroizing;

        use crate::{
            database::sealed::Sealed,
            identity::keys,
            keys::{Entry, Id, Material},
        };
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
          for case in ["number", "binding", "canonical", "𠀀人", "Á"] {
            timeout(Duration::from_secs(20),async {
                let database=Database::initialized(Arc::clone(&server)).await; let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let actor=actor(&database,&services,&mut supplier).await;
                let owner=database.connections("postgres").recover().await.expect("owner");
                let role=role::Id::allocate();
                let person=page_person(owner.client(),actor.session_id()).await;
                let revision_id:revision::Id=serde_json::from_str(r#""00000000-0000-4000-8000-000000000003""#).expect("vector revision");
                let entry=|id,text:&str|Entry::new(Id::new(id).expect("key id"),Material::new(Zeroizing::new(text.into())).expect("key material"));
                let mut keys=authentication::keys();
                keys.encryption=keys::encryption::Keys::new(crate::keys::Derived::fixed(Id::new(1).expect("active"),vec![entry(0,"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"),entry(1,"KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio")]));
                let inventory=authentication::inventory(keys);
                let fixture=kernel::App::new(services.role().app.context().clone(),Arc::clone(services.role().app.registry()),Arc::clone(services.role().app.work()),Arc::new(inventory.identity.encryption),Arc::new(inventory.seal));
                let mut service=services.role().clone(); service.revisions=revision::Reader::new(fixture.clone());
                // Reuse the independent AES-GCM-SIV vectors from the Revision owner.
                let sealed=|text|Sealed(Envelope::new(hex::decode(text).expect("vector bytes")).expect("envelope"));
                let name=sealed("010000000000000000000102030405060708090a0bf4727db6706d688a40deccc16298b31c801e765142fe1f20150eec6da4b86a72");
                let number=sealed("010000000000000000000102030405060708090a0b7fa5a373a7a56a098a6555484725c9c42e03c35c99ace908000ef9043b4b261b7def");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("historical operator fixture");
                revision::fixture_builder_sealed(fixture.keys(),revision_id,person,&"Fixture Operator".parse().expect("name"),&"000000200002290010".parse().expect("number"),revision::Seals{name,id_number:number}).expect("sealed fixture").insert(owner.client(),revision::FixtureSource::Person).await.expect("retained original version");
                owner.client().batch_execute("COMMIT").await.expect("operator recorded");
                seed_history(owner.client(),&service,role,vec![Data {
                    id:super::Id::allocate(),previous_id:None,scope:Scope::Institution,entry:None,
                    action:Action::Create {permissions:Vec::new()},name:"Past".parse().expect("fixture name"),
                    level:200.try_into().expect("fixture level"),operator_id:revision_id,
                    changed_at:context.sample().await.expect("fixture time"),
                    operation_id:Uuid::new_v4().to_string().parse().expect("fixture operation"),
                }]).await;
                let selected=||Filter::new(Pagination::new(1,20).expect("page"),Some(role),None);
                assert_eq!(service.changes(&actor,selected()).await.expect("original retired key").changes()[0].operator().name(),"F*");
                if matches!(case, "number" | "binding" | "canonical") {
                    let assignments=insert_role(owner.client(), services.role(),&actor,"Assignments",300,&["institution.assignments"]).await;
                    let full=services.role().read_current(&actor).await.expect("original full role").id();
                    let noncanonical=sealed("010000000000000000000102030405060708090a0bee91377aefb7141faeafde2475ff49bcc7d360aecbb6d2969e7b5f2e6fca0106029d");
                    let wrong=revision::Seals::seal(fixture.keys().data,revision::Id::allocate(),&"Fixture Operator".parse().expect("name"),&"000000200002290010".parse().expect("number")).expect("wrong AAD");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated ciphertext damage");
                    match case {
                        "number"=>{owner.client().execute("UPDATE identity.revisions SET id_number=set_byte(id_number,54,get_byte(id_number,54)#1) WHERE id=$1",&[&revision_id]).await.expect("valid name, invalid number authentication");}
                        "binding"=>{owner.client().execute("UPDATE identity.revisions SET name=$1,id_number=$2 WHERE id=$3",&[&wrong.name,&wrong.id_number,&revision_id]).await.expect("authentic different revision");}
                        _=>{owner.client().execute("UPDATE identity.revisions SET name=$1 WHERE id=$2",&[&noncanonical,&revision_id]).await.expect("authentic noncanonical plaintext");}
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage visible");
                    assign_role(owner.client(), services.role(),&actor,assignments).await;
                    assert!(matches!(service.changes(&actor,selected()).await,Err(kernel::Error::Denied)),"permission precedes corrupted operator");
                    services.ready().await.expect("unread damage does not close admission");
                    assign_role(owner.client(), services.role(),&actor,full).await;
                    let error=service.changes(&actor,selected()).await.err().expect("no partial identity");
                    assert!(matches!(&error, kernel::Error::Corrupt(fault) if matches!(fault.kind(), kernel::FaultKind::Role(Error::Revision(revision::ReadError::Revision(_))))),"{error:?}");
                    assert!(services.ready().await.is_err(), "observed ciphertext damage closes shared admission");
                    for secret in ["Fixture Operator","000000200002290010"] {assert!(!format!("{error:?}").contains(secret));}
                } else {
                    let expected = if case == "𠀀人" { "𠀀*" } else { "*" };
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("mask fixture");
                    revision::fixture_update(owner.client(),fixture.keys(),revision_id,person,&case.parse().expect("name"),&"000000200002290010".parse().expect("number")).await.expect("complete identity");
                    owner.client().batch_execute("COMMIT").await.expect("mask fixture ready");
                    assert_eq!(service.changes(&actor,selected()).await.expect("scalar-based mask").changes()[0].operator().name(),expected);
                }
                owner.terminate().await.expect("ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            }).await.unwrap_or_else(|error| panic!("operator case {case}: {error}"));
          }
        });
    }

    #[test]
    fn history_snapshot_survives_permission_changes_and_ends_cancelled_or_unacknowledged_reads() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::access::appoint;

        async fn appoint(
            service: &crate::access::role::Service,
            session: &mut platform::database::Session,
            actor: &crate::identity::current::Actor,
            selection: appoint::Selection,
        ) {
            let revision_id: crate::identity::person::revision::Id = session.client().query_one(
                "SELECT person.revision_id FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id WHERE account.id=$1",
                &[&actor.account_id()],
            ).await.expect("current revision").get(0);
            let input = serde_json::from_value(serde_json::json!({
                "operator": {"mobile":"13900000000"}, "accountId": actor.account_id(),
                "revisionId": revision_id, "selection": selection,
            }))
            .expect("system appointment input");
            appoint::run(
                service.app.clone(),
                Arc::new(authentication::keys()),
                crate::identity::operation::Id::allocate(),
                input,
                false,
            )
            .await
            .expect("concurrent system command")
            .expect("committed appointment");
        }
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                for case in ["snapshot", "cancel", "commit"] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) =
                        crate::access::role::tests::start_snapshot(authentication::proxied(&database, &proxy), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let owner = database
                        .connections("postgres")
                        .recover()
                        .await
                        .expect("independent observer");
                    let ordinary = services
                        .role()
                        .list(&actor, Pagination::new(1, 20).expect("page"))
                        .await
                        .expect("default role")
                        .roles()[1]
                        .id();
                    // Exercise the system command on the same application while the snapshot waits.
                    let mut maintenance = database.connections("postgres").recover().await.expect("isolated maintenance writer");
                    proxy.arm(Fault::StalledRoleHistoryCount);
                    let service = services.role().clone();
                    let current = actor.clone();
                    let task =
                        tokio::spawn(async move { service.changes(&current, filter()).await });
                    let pid = timeout(Duration::from_secs(2), proxy.intercepted())
                        .await
                        .expect("history count after authorization");
                    if case == "snapshot" {
                        let added = services.role().create(actor.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(serde_json::json!({"name":"Later","level":200,"permissions":[]})).expect("definition")).await.expect("concurrent definition").id();
                        appoint(services.role(), &mut maintenance, &actor, appoint::Selection::Revoke { role_id: ordinary }).await;
                        proxy.release();
                        let page = timeout(Duration::from_secs(2), task)
                            .await
                            .expect("page deadline")
                            .expect("task")
                            .expect("original authorization snapshot");
                        assert_eq!(
                            owner
                                .client()
                                .query_one(
                                    "SELECT count(*) FROM access.changes WHERE scope='institution'",
                                    &[]
                                )
                                .await
                                .expect("independent later facts")
                                .get::<_, i64>(0),
                            3
                        );
                        assert_eq!(page.count(), 2);
                        assert!(
                            !page
                                .changes()
                                .iter()
                                .any(|change| change.role_id() == added)
                        );
                        assert!(matches!(
                            services.role().changes(&actor, filter()).await,
                            Err(kernel::Error::Denied)
                        ));
                        appoint(services.role(), &mut maintenance, &actor, appoint::Selection::Grant).await;
                        let latest = services
                            .role()
                            .changes(&actor, filter())
                            .await
                            .expect("fresh authorized history snapshot");
                        assert_eq!(latest.count(), 3);
                        assert!(
                            latest
                                .changes()
                                .iter()
                                .any(|change| change.role_id() == added)
                        );
                    } else {
                        if case == "cancel" {
                            task.abort();
                            assert!(task.await.is_err_and(|error| error.is_cancelled()));
                        } else {
                            proxy.arm(Fault::LostReadCommit);
                            proxy.release();
                            timeout(Duration::from_secs(2), proxy.intercepted())
                                .await
                                .expect("actual read COMMIT");
                            let error = timeout(Duration::from_secs(2), task)
                                .await
                                .expect("read terminates")
                                .expect("task")
                                .err()
                                .expect("no unacknowledged page");
                            let kernel::Error::Unavailable(failure) = &error else { panic!("an unacknowledged read commit retains its technical cause: {error:?}"); };
                            assert_eq!(failure.stage(), kernel::Stage::Read);
                            assert!(matches!(failure.kind(), kernel::FaultKind::Query(_)), "{error:?}");
                        }
                        proxy.release();
                        timeout(Duration::from_secs(2), async {
                            while owner
                                .client()
                                .query_one(
                                    "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",
                                    &[&pid],
                                )
                                .await
                                .expect("backend evidence")
                                .get::<_, bool>(0)
                            {
                                tokio::task::yield_now().await;
                            }
                        })
                        .await
                        .expect("original backend ended");
                        assert_eq!(
                            owner
                                .client()
                                .query_one("SELECT count(*) FROM access.changes", &[])
                                .await
                                .expect("no business effects before another read")
                                .get::<_, i64>(0),
                            4
                        );
                        assert_eq!(
                            services
                                .role()
                                .changes(&actor, filter())
                                .await
                                .expect("fresh read")
                                .count(),
                            2
                        );
                    }
                    maintenance.terminate().await.expect("maintenance ended");
                    owner.terminate().await.expect("ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    proxy.stop().await;
                    supplier.stop().await;
                }
            })
            .await
            .expect("history resource boundaries");
        });
    }
}
