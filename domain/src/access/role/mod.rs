//! Role definitions and the qualified current institutional role projection.

pub mod change;
pub(super) mod database;
pub(super) mod maintenance;
mod operation;
mod query;
mod reader;
pub(super) mod state;
pub mod workforce;

use std::str::FromStr;

pub(crate) async fn definitions(
    client: &impl tokio_postgres::GenericClient,
    keys: crate::history::Keys<'_>,
    selections: &[(Id, change::Id)],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<Role>, Error> {
    database::definitions(client, keys, selections, Scope::Institution, now).await
}
use serde::{Deserialize, Serialize};
use serde_with::DeserializeFromStr;
use tokio_postgres::types::{FromSql, ToSql, Type};
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;
use uuid::Uuid;

pub(crate) use crate::access::role::{operation::verify, reader::Reader};
pub use crate::access::{
    capability::Code,
    role::operation::{Delete, Input, Update},
};
pub(crate) const ROLES: &str = state::TABLE;
pub(crate) const CHANGES: &str = change::TABLE;
use crate::{
    access::capability::Scope,
    identity::{current, person::revision},
    runtime,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, FromSql, ToSql)]
#[serde(transparent)]
#[schema(as = role::Id, value_type = Uuid)]
#[postgres(transparent)]
pub struct Id(Uuid);

impl std::str::FromStr for Id {
    type Err = uuid::Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(input)?))
    }
}

impl Id {
    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = role::Selection)]
pub struct Selection {
    pub(in crate::access) id: Id,
    pub(in crate::access) change_id: change::Id,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = role::Kind)]
pub enum Kind {
    Default,
    Full,
    Custom,
}

impl Kind {
    fn new(scope: Scope, name: &Name, level: Level, permissions: &[Code]) -> Result<Self, Error> {
        Permissions::validate(scope, permissions).map_err(Error::invalid_permissions)?;
        let (ordinary, full) = match scope {
            Scope::Institution => (Builtin::OrdinaryAccount, Builtin::SystemAdministrator),
            Scope::Workforce => (Builtin::OrdinaryMember, Builtin::Administrator),
        };
        match level.0 {
            1 if name.0 == ordinary.name() && permissions.is_empty() => Ok(Self::Default),
            1000 if name.0 == full.name()
                && Code::all()
                    .iter()
                    .filter(|code| code.scope() == scope)
                    .all(|code| permissions.contains(code)) =>
            {
                Ok(Self::Full)
            }
            2..=999 => Ok(Self::Custom),
            _ => Err(Error::Facts {
                rule: "role levels, immutable system names and default or full permissions agree",
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, DeserializeFromStr, ToSchema, ToSql)]
#[serde(transparent)]
#[schema(as = role::Name, value_type = String, min_length = 1, max_length = 64)]
#[postgres(transparent)]
pub struct Name(String);

impl FromStr for Name {
    type Err = InputError;

    fn from_str(value: &str) -> Result<Self, InputError> {
        let value: String = value.trim().nfc().collect();
        if !(1..=64).contains(&value.chars().count()) || value.chars().any(char::is_control) {
            return Err(InputError::Name);
        }
        Ok(Self(value))
    }
}

impl<'a> FromSql<'a> for Name {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let value = <&str>::from_sql(kind, raw)?;
        let name: Self = value.parse()?;
        if name.0 != value {
            return Err(Box::new(InputError::Name));
        }
        Ok(name)
    }
    fn accepts(kind: &Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema, ToSql,
)]
#[serde(try_from = "i16")]
#[serde(into = "i16")]
#[schema(as = role::Level, value_type = i16, minimum = 1, maximum = 1000)]
#[postgres(transparent)]
pub struct Level(i16);

impl From<Level> for i16 {
    fn from(level: Level) -> Self {
        level.0
    }
}

impl TryFrom<i16> for Level {
    type Error = InputError;

    fn try_from(value: i16) -> Result<Self, InputError> {
        if !(1..=1000).contains(&value) {
            return Err(InputError::Level);
        }
        Ok(Self(value))
    }
}

impl<'a> FromSql<'a> for Level {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self::try_from(i16::from_sql(kind, raw)?)?)
    }

    fn accepts(kind: &Type) -> bool {
        <i16 as FromSql>::accepts(kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "Vec<Code>")]
pub struct Permissions(Vec<Code>);

impl Permissions {
    fn validate(scope: Scope, values: &[Code]) -> Result<(), InputError> {
        if values
            .iter()
            .enumerate()
            .any(|(index, code)| code.scope() != scope || values[..index].contains(code))
        {
            return Err(InputError::Permissions);
        }
        Ok(())
    }
    fn schema_for(scope: Scope) -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::schema::ArrayBuilder::new()
            .items(
                utoipa::openapi::schema::ObjectBuilder::new()
                    .schema_type(utoipa::openapi::schema::Type::String)
                    .enum_values(Some(
                        Code::all()
                            .iter()
                            .filter(|code| code.scope() == scope)
                            .map(AsRef::as_ref),
                    )),
            )
            .unique_items(true)
            .into()
    }
}

impl utoipa::PartialSchema for Permissions {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        Self::schema_for(Scope::Institution)
    }
}

impl ToSchema for Permissions {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("role.Permissions")
    }
}

impl TryFrom<Vec<Code>> for Permissions {
    type Error = InputError;

    fn try_from(mut values: Vec<Code>) -> Result<Self, InputError> {
        Self::validate(Scope::Institution, &values)?;
        values.sort_unstable_by(|left, right| left.as_ref().cmp(right.as_ref()));
        Ok(Self(values))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("role name must contain 1 to 64 normalized scalars without controls")]
    Name,
    #[error("role level must be between 1 and 1000")]
    Level,
    #[error("role permissions must be distinct codes in the requested scope")]
    Permissions,
}

/// Delivered only after the original actor is confirmed in this definition's snapshot.
///
/// ```compile_fail
/// use domain::access::role::Role;
/// fn change(mut role: Role) { role.level = 1000; }
/// ```
///
/// ```compile_fail
/// use domain::access::role::Role;
/// fn accept<T: for<'de> serde::Deserialize<'de>>() {}
/// accept::<Role>();
/// ```
#[derive(Clone)]
pub struct Role {
    id: Id,
    name: Name,
    level: Level,
    kind: Kind,
    permissions: Vec<Code>,
    change_id: change::Id,
}

impl Role {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn name(&self) -> &Name {
        &self.name
    }
    pub fn level(&self) -> Level {
        self.level
    }
    pub fn kind(&self) -> Kind {
        self.kind
    }
    pub fn permissions(&self) -> &[Code] {
        &self.permissions
    }
    pub fn change_id(&self) -> change::Id {
        self.change_id
    }
}

/// One authorized snapshot's count and complete page of definitions.
pub struct List {
    count: i64,
    roles: Vec<Role>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn roles(&self) -> &[Role] {
        &self.roles
    }
}

#[derive(Clone)]
pub struct Service {
    app: crate::kernel::App,
    current: current::Service,
    roles: Reader,
    revisions: revision::Reader,
}

impl Service {
    pub(crate) fn prepare(
        app: crate::kernel::App,
        current: current::Service,
        revisions: revision::Reader,
    ) -> Self {
        Self {
            roles: Reader::new(app.clone()),
            app,
            current,
            revisions,
        }
    }
}

/// The executor carries these role-specific outcomes alongside shared technical failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    NameTaken,
    Absent,
    Changed,
    Unchanged,
    InUse,
}

impl crate::kernel::Rejection for Rejection {
    fn status(&self) -> crate::kernel::Status {
        match self {
            Self::Absent => crate::kernel::Status::NotFound,
            _ => crate::kernel::Status::Conflict,
        }
    }
    fn code(&self) -> &'static str {
        match self {
            Self::NameTaken => "role_name_taken",
            Self::Absent => "target_unavailable",
            Self::Changed => "role_changed",
            Self::Unchanged => "state_conflict",
            Self::InUse => "role_in_use",
        }
    }
}

pub type CommandError = crate::kernel::Error<Rejection>;

impl From<Rejection> for CommandError {
    fn from(value: Rejection) -> Self {
        Self::Rejected(value)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("role definition access is denied")]
    Denied,
    #[error("role input differs from the original operation")]
    Mismatch,
    #[error("the role name is already in use in this scope")]
    NameTaken,
    #[error("the institutional role is unavailable")]
    Absent,
    #[error("the confirmed role definition has changed")]
    Changed,
    #[error("the role definition is unchanged")]
    Unchanged,
    #[error("the institutional role has current holders")]
    InUse,
    #[error("{0}")]
    Current(#[from] current::Error),
    #[error("{0}")]
    Membership(#[source] Box<crate::access::grant::Error>),
    #[error("{0}")]
    Revision(#[from] revision::ReadError),
    #[error("{0}")]
    History(#[from] crate::history::Error),
    #[error("role fact check failed: {rule}")]
    Facts { rule: &'static str },
    #[error("role query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("role decoding failed: {0}")]
    Decode(#[source] crate::database::Query),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("{0}")]
    Work(#[from] runtime::work::Error),
    #[error("role transaction coordination failed")]
    Crypto(#[from] platform::cryptography::Error),
    #[error("role transaction end failed: {0}")]
    End(#[source] crate::database::transaction::EndError),
    #[error("original role operation did not commit")]
    NotCommitted {
        #[source]
        cause: Box<Error>,
        commit: Result<(), crate::database::transaction::EndError>,
        termination: Result<(), platform::database::Error>,
    },
    #[error("original role operation outcome remains unknown")]
    Unknown {
        #[source]
        cause: Box<Error>,
        recovery: Box<Error>,
    },
    #[error("role query budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("role rejection rollback failed")]
    Rollback {
        cause: Box<Error>,
        rollback: crate::database::Query,
    },
    #[error("role rejection rollback exceeded the transaction budget")]
    RollbackTimeout {
        cause: Box<Error>,
        elapsed: tokio::time::error::Elapsed,
    },
    #[error("role query and connection termination failed")]
    Combined {
        cause: Box<Error>,
        termination: platform::database::Error,
    },
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        if error.as_db_error().is_some_and(|error| {
            error.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION
                && error.schema() == Some("access")
                && error.table() == Some("roles")
                && error.constraint() == Some("roles_scope_name_key")
        }) {
            Self::NameTaken
        } else if error.as_db_error().is_some_and(|error| {
            error.code() == &tokio_postgres::error::SqlState::FOREIGN_KEY_VIOLATION
                && error.schema() == Some("access")
                && matches!(
                    (error.table(), error.constraint()),
                    (Some("holders"), Some("holders_role_id_scope_fkey"))
                        | (Some("members"), Some("members_role_id_scope_fkey"))
                )
        }) {
            Self::InUse
        } else {
            Self::Query(error.into())
        }
    }
}

impl crate::database::read::Failure for Error {
    fn changed(&self) -> bool {
        crate::database::read::changed(self)
    }
}

impl From<crate::access::grant::Error> for Error {
    fn from(error: crate::access::grant::Error) -> Self {
        match error {
            crate::access::grant::Error::Denied => Self::Denied,
            crate::access::grant::Error::Current(error) => Self::Current(error),
            crate::access::grant::Error::Role(error) => error,
            error => Self::Membership(Box::new(error)),
        }
    }
}

impl Error {
    fn decode(error: tokio_postgres::Error) -> Self {
        Self::Decode(error.into())
    }

    fn invalid_permissions(_: InputError) -> Self {
        Self::Facts {
            rule: "role permissions are distinct and belong to its scope",
        }
    }

    fn rejected(&self) -> bool {
        matches!(
            self,
            Self::Denied
                | Self::Mismatch
                | Self::NameTaken
                | Self::Absent
                | Self::Changed
                | Self::Unchanged
                | Self::InUse
                | Self::Current(current::Error::Invalid)
        )
    }

    pub fn failure(&self) -> Failure {
        match self {
            Self::Denied => Failure::Denied,
            Self::Mismatch => Failure::Mismatch,
            Self::NameTaken => Failure::NameTaken,
            Self::Absent => Failure::Absent,
            Self::Changed => Failure::Changed,
            Self::Unchanged => Failure::Unchanged,
            Self::InUse => Failure::InUse,
            Self::Current(current::Error::Invalid) => Failure::Invalid,
            Self::Work(runtime::work::Error::Busy) => Failure::Busy,
            Self::Work(runtime::work::Error::Worker) => Failure::Worker,
            Self::Unknown { .. } => Failure::Unknown,
            _ => Failure::Unavailable,
        }
    }
}

impl From<crate::database::transaction::Error> for Error {
    fn from(error: crate::database::transaction::Error) -> Self {
        match error {
            crate::database::transaction::Error::Query(error) => Self::Query(error),
            crate::database::transaction::Error::Crypto(error) => Self::Crypto(error),
            // The fence already closed admission; the outcome stays with the next process.
            crate::database::transaction::Error::Superseded => {
                Self::Work(runtime::work::Error::Closed)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum Failure {
    Invalid,
    Denied,
    Mismatch,
    NameTaken,
    Absent,
    Changed,
    Unchanged,
    InUse,
    Busy,
    Worker,
    Unavailable,
    Unknown,
}

impl runtime::work::Failure for Error {
    fn unreliable(&self) -> bool {
        match self {
            Self::History(error) => error.corrupt(),
            Self::Facts { .. } | Self::Decode(_) => true,
            Self::Revision(error) => error.unreliable(),
            Self::Current(error) => error.unreliable(),
            Self::Membership(error) => error.unreliable(),
            Self::Combined { cause, .. }
            | Self::Rollback { cause, .. }
            | Self::RollbackTimeout { cause, .. }
            | Self::NotCommitted { cause, .. } => cause.unreliable(),
            Self::Unknown { cause, recovery } => cause.unreliable() || recovery.unreliable(),
            _ => false,
        }
    }

    fn report(&self, operation_id: crate::identity::operation::Id) {
        if !self.rejected() {
            tracing::error!(%operation_id, stage = "role_completion", kind = self.failure().as_ref(), error = ?self);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Builtin {
    OrdinaryAccount,
    SystemAdministrator,
    OrdinaryMember,
    Administrator,
}

impl Builtin {
    pub(crate) const ALL: [Self; 4] = [
        Self::OrdinaryAccount,
        Self::SystemAdministrator,
        Self::OrdinaryMember,
        Self::Administrator,
    ];

    pub(crate) fn scope(self) -> Scope {
        match self {
            Self::OrdinaryAccount | Self::SystemAdministrator => Scope::Institution,
            Self::OrdinaryMember | Self::Administrator => Scope::Workforce,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::OrdinaryAccount => "普通账户",
            Self::SystemAdministrator => "系统管理员",
            Self::OrdinaryMember => "普通成员",
            Self::Administrator => "管理员",
        }
    }

    pub(crate) fn level(self) -> i16 {
        match self {
            Self::OrdinaryAccount | Self::OrdinaryMember => 1,
            Self::SystemAdministrator | Self::Administrator => 1000,
        }
    }
}
/// The public registration operation reads the institutional role under its scope guard.
pub(crate) async fn lock_institution(
    client: &tokio_postgres::Client,
) -> Result<(), crate::database::transaction::Error> {
    crate::database::transaction::shared(client, b"access/scope", b"institution").await
}

pub(in crate::access) async fn fetch_administrator(
    client: &tokio_postgres::Client,
    roles: &Reader,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Role, Error> {
    let states = roles.all(client, now).await?;
    states
        .iter()
        .find(|state| {
            state.active()
                && state.scope() == Scope::Institution
                && state.role().kind() == Kind::Full
        })
        .map(|state| state.role().clone())
        .ok_or(Error::Facts {
            rule: "a retained institution has its administrator role",
        })
}

pub(crate) async fn fetch_default_institution(
    client: &tokio_postgres::Client,
    roles: &Reader,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Role, Error> {
    let states = roles.all(client, now).await?;
    states
        .iter()
        .find(|state| {
            state.active()
                && state.scope() == Scope::Institution
                && state.role().kind() == Kind::Default
        })
        .map(|state| state.role().clone())
        .ok_or(Error::Facts {
            rule: "registration retains its default institutional role",
        })
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use chrono::TimeDelta;
    use tokio::{runtime::Runtime, time::timeout};
    use tokio_postgres::types::{FromSql, Type};
    use utoipa::PartialSchema;
    use uuid::Uuid;

    use crate::{
        access::role::{Code, Id, Kind, Name, change},
        database::Barrier,
        identity::{
            authentication::{
                self, Database, Supplier, keys, server,
                session::{
                    Receipt,
                    tests::{CODE, input, message},
                },
            },
            current::{Actor, Proof},
        },
        kernel,
        pagination::Pagination,
        runtime::Context,
        startup::{Services, Settings},
    };

    const PERMISSIONS: [&str; 7] = [
        "institution.accounts",
        "institution.affiliations.correct",
        "institution.affiliations.read",
        "institution.assignments",
        "institution.corrections",
        "institution.roles",
        "institution.workforce",
    ];

    pub(crate) async fn start(
        connections: platform::database::Connections,
        supplier: &Supplier,
    ) -> (Context, Services) {
        let context = Context::new(connections).await.expect("context");
        start_context(context, supplier).await
    }

    pub(crate) async fn start_snapshot(
        connections: platform::database::Connections,
        supplier: &Supplier,
    ) -> (Context, Services) {
        let context = Context::with_read_budget(connections, Duration::from_secs(30))
            .await
            .expect("snapshot context");
        start_context(context, supplier).await
    }

    async fn start_context(context: Context, supplier: &Supplier) -> (Context, Services) {
        let services = Box::pin(Services::start(
            Barrier::begin(context.clone()).await.expect("barrier"),
            authentication::inventory(keys()),
            Settings::new(
                4,
                crate::identity::message::Limits::default(),
                authentication::session_settings(),
                crate::registration::Limits::default(),
                authentication::avatar_settings(),
            )
            .expect("settings"),
            supplier.client(),
            authentication::unused_client().await,
        ))
        .await
        .expect("verified services");
        (context, services)
    }

    pub(crate) async fn actor(
        database: &Database,
        services: &Services,
        supplier: &mut Supplier,
    ) -> Actor {
        let id = message(database, services.authentication(), supplier, "13900000000").await;
        let Receipt::Session(issued) = Box::pin(services.authentication().create_session(input(
            Uuid::new_v4(),
            id,
            "13900000000",
            CODE,
        )))
        .await
        .expect("actual initialized R1") else {
            panic!("initialized account");
        };
        services
            .current()
            .authenticate(Proof {
                id: issued.id(),
                token: issued.token().text().parse().expect("token"),
            })
            .await
            .expect("authenticated actor")
    }

    /// Rebuild a complete, legitimately sealed disposable history, then install its final anchor.
    pub(super) async fn seed_history(
        client: &tokio_postgres::Client,
        service: &super::Service,
        id: Id,
        records: Vec<change::Data>,
    ) {
        use super::{
            Input, Permissions, Role,
            operation::{self, Action},
            state,
        };
        use crate::{
            access::capability::Scope,
            history::{Change, Draft, Seed},
            kernel::{Authorization, completion},
        };
        client
            .batch_execute("BEGIN; SET LOCAL session_replication_role=replica")
            .await
            .expect("isolated history fixture");
        client.execute("DELETE FROM access.completions completion USING access.changes change WHERE change.role_id=$1 AND change.operation_id=completion.operation_id AND completion.family=CASE change.scope WHEN 'institution' THEN 'institution_role' ELSE 'workforce_role' END", &[&id]).await.expect("replace fixture completions");
        client
            .execute("DELETE FROM access.changes WHERE role_id=$1", &[&id])
            .await
            .expect("replace fixture history");
        client
            .execute("DELETE FROM access.roles WHERE id=$1", &[&id])
            .await
            .expect("replace fixture state");
        let mut previous: Option<crate::history::Verified<state::State>> = None;
        for mut record in records {
            record.previous_id = previous.as_ref().map(|state| state.role().change_id());
            let permissions = match &record.action {
                change::Action::Create { permissions } | change::Action::Update { permissions } => {
                    permissions.clone()
                }
                change::Action::Delete => Vec::new(),
            };
            let kind = Kind::new(record.scope, &record.name, record.level, &permissions)
                .expect("fixture kind");
            let active = !matches!(record.action, change::Action::Delete);
            let role = Role {
                id,
                name: record.name.clone(),
                level: record.level,
                kind,
                permissions: permissions.clone(),
                change_id: record.id,
            };
            let data = state::Data {
                role,
                scope: record.scope,
                active,
                changed_at: record.changed_at,
            };
            let draft = match &previous {
                None => Draft::create(data),
                Some(state) => Draft::advance(state, data).expect("fixture successor"),
            };
            if kind == Kind::Custom {
                let action = match record.scope {
                    Scope::Institution => {
                        let input = Input::new(
                            record.name.clone(),
                            record.level,
                            Permissions::try_from(permissions).expect("fixture permissions"),
                        );
                        match record.previous_id {
                            None => Action::Create(input),
                            Some(change_id) if active => Action::Update {
                                id,
                                change_id,
                                input,
                            },
                            Some(change_id) => Action::Delete { id, change_id },
                        }
                    }
                    Scope::Workforce => {
                        let entry = record.entry.expect("fixture entry");
                        let input = super::workforce::Input {
                            entry,
                            name: record.name.clone(),
                            level: record.level,
                            permissions: permissions
                                .try_into()
                                .expect("fixture scoped permissions"),
                        };
                        match record.previous_id {
                            None => Action::CreateWorkforce(input),
                            Some(change_id) if active => Action::UpdateWorkforce {
                                id,
                                change_id,
                                input,
                            },
                            Some(change_id) => Action::DeleteWorkforce {
                                id,
                                change_id,
                                entry,
                            },
                        }
                    }
                };
                let operator = service
                    .revisions
                    .fetch_identity(client, record.operator_id)
                    .await
                    .expect("fixture fixed operator");
                let authority = Authorization {
                    person_id: operator.person_id(),
                    revision_id: record.operator_id,
                    now: record.changed_at,
                };
                let request = action.request(&authority).expect("fixture canonical input");
                let template = if record.scope == Scope::Institution {
                    operation::INSTITUTION
                } else {
                    operation::WORKFORCE
                };
                completion::insert::<super::Rejection>(
                    client,
                    service.app.keys(),
                    template,
                    record.operation_id,
                    &request,
                    record.changed_at,
                    template.expires(record.changed_at),
                )
                .await
                .expect("fixture completion");
            }
            previous = Some(
                Seed::write(client, service.app.keys(), Change::new(draft, record))
                    .await
                    .expect("paired fixture state and record"),
            );
        }
        client
            .batch_execute("COMMIT")
            .await
            .expect("complete fixture history");
        Seed::anchor(
            service.app.anchors(),
            &previous.expect("nonempty role history"),
        )
        .expect("fixture anchor");
    }

    pub(super) async fn insert_role(
        client: &tokio_postgres::Client,
        service: &super::Service,
        actor: &Actor,
        name: &str,
        level: i16,
        permissions: &[&str],
    ) -> Id {
        let id = Id::allocate();
        let operator_id = client.query_one("SELECT person.revision_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1", &[&actor.session_id()])
            .await.expect("fixture operator").get(0);
        let input = super::Input::new(
            name.parse().expect("fixture name"),
            level.try_into().expect("fixture level"),
            permissions
                .iter()
                .map(|code| code.parse().expect("fixture code"))
                .collect::<Vec<_>>()
                .try_into()
                .expect("fixture permissions"),
        );
        let now = service.app.context().sample().await.expect("fixture time");
        seed_history(
            client,
            service,
            id,
            vec![change::Data {
                id: change::Id::allocate(),
                previous_id: None,
                scope: super::Scope::Institution,
                entry: None,
                action: change::Action::Create {
                    permissions: input.permissions.0,
                },
                name: input.name,
                level: input.level,
                operator_id,
                changed_at: now,
                operation_id: Uuid::new_v4()
                    .to_string()
                    .parse()
                    .expect("fixture operation"),
            }],
        )
        .await;
        id
    }

    pub(super) async fn assign_role(
        client: &tokio_postgres::Client,
        service: &super::Service,
        actor: &Actor,
        role_id: Id,
    ) {
        use crate::{
            access::holder,
            history::{self, Read, Seed},
        };
        let now = service.app.context().sample().await.expect("fixture time");
        client
            .batch_execute("BEGIN; SET LOCAL session_replication_role=replica")
            .await
            .expect("isolated assignment fixture");
        let row = client.query_one(
            "SELECT account.id,person.revision_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1",
            &[&actor.session_id()],
        ).await.expect("fixture subject");
        let state = history::find::<holder::State>(
            client,
            service.app.keys(),
            service.app.anchors(),
            Read::Locked,
            row.get("id"),
            now,
        )
        .await
        .expect("authenticated holder")
        .expect("fixture holder");
        let role = service
            .roles
            .find(client, role_id, super::Scope::Institution, now)
            .await
            .expect("selected role")
            .expect("fixture role");
        // Like seed_history, this fixture supplies authenticated facts, not a replayable command.
        let change = holder::replace(
            &state,
            &role,
            holder::Assignment {
                id: crate::access::appointment::Id::allocate(),
                previous_id: Some(state.assignment_id()),
                change_id: role.change_id(),
                operator_id: row.get("revision_id"),
                revision_id: row.get("revision_id"),
                source: holder::Source::System,
            },
            Uuid::new_v4()
                .to_string()
                .parse()
                .expect("fixture operation"),
            now,
        )
        .expect("fixture replacement");
        let state = Seed::write(client, service.app.keys(), change)
            .await
            .expect("paired holder fixture");
        client
            .batch_execute("COMMIT")
            .await
            .expect("complete holder fixture");
        Seed::anchor(service.app.anchors(), &state).expect("fixture holder anchor");
    }
    #[test]
    fn role_list_checks_exact_permission_and_returns_complete_ordered_pages() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let owner = database.connections("postgres").recover().await.expect("isolated fixture owner");
                let first = insert_role(owner.client(), services.role(), &actor, "Alpha", 200, &["institution.roles"]).await;
                let second = insert_role(owner.client(), services.role(), &actor, "Beta", 200, &[]).await;
                let assignments = insert_role(owner.client(), services.role(), &actor, "Assignments", 300, &["institution.assignments"]).await;
                let facts = owner.client().query(r#"SELECT id,change_id,name,level FROM access.roles WHERE scope='institution' ORDER BY level DESC,name COLLATE "C",id"#, &[]).await.expect("independent definition order");
                assert_eq!(facts.len(), 5);
                let all = services.role().list(&actor, Pagination::new(1, 100).expect("maximum page")).await.expect("full list");
                assert_eq!(all.count(), 5);
                assert_eq!(all.roles().len(), facts.len());
                for (role, row) in all.roles().iter().zip(&facts) {
                    assert_eq!(role.id(), row.get("id"));
                    assert_eq!(role.change_id(), row.get("change_id"));
                    assert_eq!(i16::from(role.level()), row.get::<_, i16>("level"));
                    assert_eq!(serde_json::to_value(role.name()).expect("name"), row.get::<_, String>("name"));
                }
                assert_eq!(all.roles().iter().map(|role| role.kind()).collect::<Vec<_>>(), [Kind::Full,Kind::Custom,Kind::Custom,Kind::Custom,Kind::Default]);
                assert_eq!((all.roles()[2].id(), all.roles()[3].id()), (first, second));
                let page = services.role().list(&actor, Pagination::new(2, 2).expect("middle page")).await.expect("middle list");
                assert_eq!(page.count(), 5);
                assert_eq!(page.roles().iter().map(|role| role.id()).collect::<Vec<_>>(), [first, second]);
                for number in [4, i64::MAX] {
                    let page = services.role().list(&actor, Pagination::new(number, 1).expect("page")).await.expect("page read");
                    assert_eq!(page.count(), 5);
                    assert_eq!(page.roles().len(), usize::from(number == 4));
                }
                assign_role(owner.client(), services.role(), &actor, first).await;
                let page = services.role().list(&actor, Pagination::new(1, 20).expect("page")).await.expect("exact permission reads every level");
                assert_eq!(i16::from(page.roles().first().expect("full role").level()), 1000);
                assert_eq!(page.count(), 5);
                assign_role(owner.client(), services.role(), &actor, assignments).await;
                assert!(matches!(services.role().list(&actor, Pagination::new(1, 20).expect("page")).await, Err(kernel::Error::Denied)));
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("damage another role");
                owner.client().execute("UPDATE access.roles SET permissions=ARRAY[]::text[] WHERE id=$1", &[&first]).await.expect("alter sealed page permissions");
                owner.client().batch_execute("COMMIT").await.expect("isolated damage");
                assert!(matches!(services.role().list(&actor, Pagination::new(1, 20).expect("page")).await, Err(kernel::Error::Denied)), "no target facts exposed before permission");
                assign_role(owner.client(), services.role(), &actor, all.roles()[0].id()).await;
                let expires_at: chrono::DateTime<chrono::Utc> = owner.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1", &[&actor.session_id()]).await.expect("original session lifetime").get(0);
                let elapsed=expires_at-database.now().await+TimeDelta::seconds(1);
                crate::registration::tests::age_facts(&database,&services,elapsed).await;
                assert!(matches!(services.role().list(&actor, Pagination::new(1, 20).expect("page")).await, Err(kernel::Error::Unauthenticated)), "identity failure precedes damaged page");
                services.ready().await.expect("undisclosed page damage does not close admission");
                crate::registration::tests::age_facts(&database,&services,-elapsed).await;
                assert!(matches!(services.role().list(&actor, Pagination::new(1, 20).expect("page")).await, Err(error) if error.is_corrupt()), "no partial page after damage");
                assert!(services.ready().await.is_err(), "observed page damage closes admission");
                owner.terminate().await.expect("owner ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            }).await.expect("role list budget");
        });
    }

    #[test]
    fn role_pages_close_on_damage_but_query_repair_preserves_new_work() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (surface, fault) in [("history", "identity"), ("list", "range"), ("history", "range"), ("list", "duplicates"), ("history", "duplicates"), ("list", "time"), ("list", "capability"), ("history", "capability"), ("list", "caller"), ("list", "page"), ("history", "caller"), ("history", "page"), ("list", "query"), ("history", "query")] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let input = |name| serde_json::from_value(serde_json::json!({"name":name,"level":20,"permissions":[]})).expect("typed role");
                    let target = Box::pin(services.role().create(actor.clone(), Uuid::new_v4().to_string().parse().expect("operation"), input("Target"))).await.expect("real custom role");
                    let owner = database.connections("postgres").recover().await.expect("isolated fault injector");
                    let count: i64 = owner.client().query_one("SELECT count(*) FROM access.changes", &[]).await.expect("independent original count").get(0);
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated post-startup fault");
                    let table = if surface == "list" { "access.roles" } else { "identity.revisions" };
                    match fault {
                        "identity" => {
                            owner.client().batch_execute("ALTER TABLE identity.revisions ALTER COLUMN name DROP NOT NULL").await.expect("isolate absent historical identity field");
                            owner.client().execute("UPDATE identity.revisions SET name=NULL WHERE id=(SELECT operator_id FROM access.changes WHERE id=$1)", &[&target.change_id()]).await.expect("missing fixed operator name");
                        }
                        "range" if surface == "list" => { owner.client().execute("UPDATE access.roles SET changed_at='280000-01-01 00:00:00+00' WHERE id=$1", &[&target.id()]).await.expect("finite state time beyond the decoder range"); }
                        "range" => { owner.client().execute("UPDATE access.changes SET changed_at='280000-01-01 00:00:00+00' WHERE id=$1", &[&target.change_id()]).await.expect("finite history time beyond the decoder range"); }
                        "duplicates" => {
                            if surface == "list" {
                                owner.client().execute("UPDATE access.roles SET permissions=ARRAY['institution.accounts','institution.accounts'] WHERE id=$1", &[&target.id()]).await.expect("duplicated current permissions");
                            } else {
                                owner.client().execute("UPDATE access.changes SET permissions=ARRAY['institution.accounts','institution.accounts'] WHERE id=$1", &[&target.change_id()]).await.expect("duplicated historical permissions");
                            }
                        }
                        "time" => {
                            owner.client().batch_execute("ALTER TABLE access.roles ALTER COLUMN changed_at DROP NOT NULL").await.expect("isolate absent required state value");
                            owner.client().execute("UPDATE access.roles SET changed_at=NULL WHERE id=$1", &[&target.id()]).await.expect("missing state definition time");
                        }
                        "capability" => {
                            if surface == "list" {
                                owner.client().execute("UPDATE access.roles SET permissions=ARRAY['private-page-value'] WHERE id=$1", &[&target.id()]).await.expect("malformed current permission");
                            } else {
                                owner.client().execute("UPDATE access.changes SET permissions=ARRAY['private-page-value'] WHERE id=$1", &[&target.change_id()]).await.expect("malformed historical permission");
                            }
                        }
                        "caller" => { owner.client().execute("DELETE FROM access.holders WHERE id=$1", &[&actor.account_id()]).await.expect("damaged reader qualification"); }
                        "page" if surface == "list" => {
                            owner.client().batch_execute("ALTER TABLE access.roles DROP CONSTRAINT roles_name_check").await.expect("isolate typed state corruption");
                            owner.client().execute("UPDATE access.roles SET name=' private-page-value ' WHERE id=$1", &[&target.id()]).await.expect("malformed target name");
                        }
                        "page" => { owner.client().execute("UPDATE identity.revisions SET name=set_byte(name,0,255) WHERE id=(SELECT operator_id FROM access.changes WHERE id=$1)", &[&target.change_id()]).await.expect("malformed fixed identity envelope"); }
                        "query" => { owner.client().batch_execute(&format!("REVOKE SELECT ON {table} FROM {}", database.user)).await.expect("ordinary statement failure"); }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("fault visible independently");
                    let selected = || change::Filter::new(Pagination::new(1, 20).expect("page"), Some(target.id()), None);
                    let error = if surface == "list" { services.role().list(&actor, Pagination::new(1, 20).expect("page")).await.err() } else { services.role().changes(&actor, selected()).await.err() }.expect("incomplete page withheld");
                    let damaged = fault != "query";
                    assert_eq!(crate::runtime::work::Failure::unreliable(&error), damaged, "{surface} {fault}: {error:?}");
                    assert_eq!(services.ready().await.is_err(), damaged);
                    assert!(!format!("{error:?}").contains("private-page-value"));
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.changes", &[]).await.expect("read has no history effects").get::<_, i64>(0), count);
                    if !damaged {
                        owner.client().batch_execute(&format!("GRANT SELECT ON {table} TO {}", database.user)).await.expect("restore disposable permission");
                        if surface == "list" { assert_eq!(services.role().list(&actor, Pagination::new(1, 20).expect("page")).await.expect("list recovered").count(), 3); }
                        else { assert_eq!(services.role().changes(&actor, selected()).await.expect("history recovered").count(), 1); }
                    }
                    let result = Box::pin(services.role().create(actor, Uuid::new_v4().to_string().parse().expect("later operation"), input("After read failure"))).await;
                    if damaged { assert!(matches!(result, Err(error) if error.is_closed())); }
                    else { result.expect("ordinary query failure preserves new work"); }
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.changes", &[]).await.expect("independent subsequent effects").get::<_, i64>(0), count + i64::from(!damaged));
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("role page {surface} {fault}: {error}"));
            }
        }));
    }

    #[test]
    fn role_list_authorization_count_and_records_share_one_snapshot() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                for retry in [true, false] {
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
                        .expect("independent fixture owner");
                    let manager = actor;
                    let actor = crate::access::appointment::tests::register(&database, &services, &mut supplier).await;
                    let reader = services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(serde_json::json!({"name":"Reader","level":200,"permissions":["institution.roles"]})).expect("reader role")).await.expect("reader role created");
                    let denied = services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(serde_json::json!({"name":"Assignments","level":200,"permissions":["institution.assignments"]})).expect("denied role")).await.expect("denied role created");
                    let candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("reader candidate");
                    services.appointment().assign(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), crate::access::appointment::tests::command(&candidate, &reader)).await.expect("reader appointed");
                    let candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("current reader candidate");
                    proxy.arm(if retry {
                        Fault::StalledRoleStates
                    } else {
                        Fault::PausedReadEnd
                    });
                    let service = services.role().clone();
                    let current = actor.clone();
                    let task = tokio::spawn(async move {
                        service
                            .list(&current, Pagination::new(1, 100).expect("page"))
                            .await
                    });
                    timeout(Duration::from_secs(2), proxy.intercepted())
                        .await
                        .expect("before state validation or after the validated snapshot ends");
                    let added = services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(serde_json::json!({"name":"Added","level":999,"permissions":[]})).expect("definition")).await.expect("concurrent definition").id();
                    services.appointment().assign(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), crate::access::appointment::tests::command(&candidate, &denied)).await.expect("concurrent permission loss");
                    proxy.release();
                    let result = timeout(Duration::from_secs(2), task)
                        .await
                        .expect("read finishes")
                        .expect("task");
                    assert_eq!(
                        owner
                            .client()
                            .query_one(
                                "SELECT count(*) FROM access.roles WHERE scope='institution'",
                                &[]
                            )
                            .await
                            .expect("independent changed total")
                            .get::<_, i64>(0),
                        5
                    );
                    if retry {
                        assert!(matches!(result, Err(kernel::Error::Denied)), "a changed anchor restarts authorization");
                    } else {
                        let page = result.expect("original validated snapshot");
                        assert_eq!(page.count(), 4);
                        assert_eq!(page.roles().len(), 4);
                        assert!(!page.roles().iter().any(|role| role.id() == added));
                    }
                    assert!(
                        matches!(
                            services
                                .role()
                                .list(&actor, Pagination::new(1, 100).expect("page"))
                                .await,
                            Err(kernel::Error::Denied)
                        ),
                        "next read sees lost permission"
                    );
                    let candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("reader to restore");
                    services.appointment().assign(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), crate::access::appointment::tests::command(&candidate, &reader)).await.expect("reader restored");
                    let page = services
                        .role()
                        .list(&actor, Pagination::new(1, 100).expect("page"))
                        .await
                        .expect("later snapshot");
                    assert_eq!(page.count(), 5);
                    assert!(page.roles().iter().any(|role| role.id() == added));
                    owner.terminate().await.expect("owner ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    proxy.stop().await;
                    supplier.stop().await;
                }
            })
            .await
            .expect("snapshot interleavings");
        });
    }

    #[test]
    fn role_list_ends_cancelled_timed_out_and_unacknowledged_transactions() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::access::capability::Scope;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for (scope, fault) in [
                (Scope::Institution, "cancel"), (Scope::Institution, "budget"),
                (Scope::Institution, "commit"), (Scope::Institution, "finish"),
                (Scope::Workforce, "cancel"), (Scope::Workforce, "budget"),
                (Scope::Workforce, "commit"), (Scope::Workforce, "finish"),
            ] {
                timeout(Duration::from_secs(25), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let owner = database.connections("postgres").recover().await.expect("independent observer");
                    // A second transaction must not be mistaken for the intercepted reader.
                    let unrelated = database.connections(&database.user).recover().await.expect("unrelated runtime connection");
                    unrelated.client().batch_execute("BEGIN; SELECT pg_catalog.pg_advisory_xact_lock(2147483647, 2147483647)").await.expect("unrelated transaction retains its own lock");
                    let unrelated_pid: i32 = unrelated.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("unrelated backend identity").get(0);
                    proxy.arm(Fault::StalledRoleStates);
                    let service = services.role().clone();
                    let current = actor.clone();
                    let task = tokio::spawn(async move { service.fetch_list(&current, Pagination::new(1, 20).expect("page"), scope).await });
                    let pid = timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("state collection held after identity and permission");
                    assert_ne!(pid, unrelated_pid);
                    if fault == "cancel" {
                        task.abort();
                        assert!(task.await.is_err_and(|error| error.is_cancelled()));
                    } else {
                        if fault == "budget" {
                            // State loading and read COMMIT share the original transaction deadline.
                            tokio::time::sleep(Duration::from_millis(1500)).await;
                            proxy.arm(Fault::StalledReadCommit);
                        } else {
                            proxy.arm(if fault == "commit" { Fault::LostReadCommit } else { Fault::StalledReadCommit });
                        }
                        proxy.release();
                        let ending_pid = timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual read COMMIT boundary");
                        assert_eq!(ending_pid, pid, "the same read transaction reaches its end");
                        let budget = if fault == "budget" { Duration::from_millis(2300) } else { Duration::from_secs(4) };
                        let error = timeout(budget, task).await.expect("one finite page deadline").expect("reader task").err().expect("no incomplete list");
                        let kernel::Error::Unavailable(failure) = &error else { panic!("read failure keeps its technical cause: {error:?}"); };
                        assert_eq!(failure.stage(), kernel::Stage::Read);
                        if fault == "commit" {
                            assert!(matches!(failure.kind(), kernel::FaultKind::Query(_)), "{error:?}");
                        }
                        else { assert!(matches!(failure.kind(), kernel::FaultKind::Timeout(_)), "{error:?}"); }
                    }
                    proxy.release();
                    timeout(Duration::from_secs(2), async {
                        while owner.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("backend observation").get::<_, bool>(0) {
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("original backend ended instead of returning to the pool");
                    let page = services.role().fetch_list(&actor, Pagination::new(1, 20).expect("page"), scope).await.expect("fresh read after failed attempt");
                    assert_eq!(page.count(), 2);
                    assert_eq!(page.roles().len(), 2);
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.changes", &[]).await.expect("no business writes or recovery replay").get::<_, i64>(0), 4);
                    assert_eq!(owner.client().query_one("SELECT state FROM pg_stat_activity WHERE pid=$1", &[&unrelated_pid]).await.expect("unrelated transaction survives reader cleanup").get::<_, String>(0), "idle in transaction");
                    unrelated.client().batch_execute("ROLLBACK").await.expect("unrelated transaction ended");
                    unrelated.terminate().await.expect("unrelated connection ended");
                    owner.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await; supplier.stop().await;
                }).await.expect("each list failure boundary");
            }
        });
    }

    #[test]
    #[tracing_test::traced_test]
    fn role_list_reuses_connections_for_denials_and_preserves_rollback_failures() {
        use fixture::database_proxy::{Fault, Proxy};
        use tracing::Instrument;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                for fault in ["none", "rejected", "stalled"] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let owner = database.connections("postgres").recover().await.expect("independent observer");
                    let page = services.role().list(&actor, Pagination::new(1, 20).expect("page")).await.expect("warm role connection");
                    assign_role(owner.client(), services.role(), &actor, page.roles()[1].id()).await;
                    if fault == "none" {
                        let pids = "SELECT array_agg(pid ORDER BY pid),bool_and(state='idle') FROM pg_stat_activity WHERE datname=current_database() AND usename=$1";
                        let before: Vec<i32> = owner.client().query_one(pids, &[&database.user]).await.expect("original pool and clock connections").get(0);
                        for _ in 0..3 {
                            assert!(matches!(services.role().list(&actor, Pagination::new(1, 20).expect("page")).await, Err(kernel::Error::Denied)));
                        }
                        let expires_at:chrono::DateTime<chrono::Utc>=owner.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1",&[&actor.session_id()]).await.expect("original session lifetime").get(0);
                        crate::registration::tests::age_facts(&database,&services,expires_at-database.now().await+TimeDelta::seconds(1)).await;
                        assert!(matches!(services.role().list(&actor, Pagination::new(1, 20).expect("page")).await, Err(kernel::Error::Unauthenticated)));
                        let after = owner.client().query_one(pids, &[&database.user]).await.expect("confirmed idle sessions");
                        assert_eq!(after.get::<_, Vec<i32>>(0), before, "expected 401 and 403 reuse the same database backends");
                        assert!(after.get::<_, bool>(1), "no open transaction remains after rejection");
                    } else {
                        proxy.arm(Fault::StalledCurrent);
                        let service = services.role().clone();
                        let task = tokio::spawn(async move { service.list(&actor, Pagination::new(1, 20).expect("page")).await }.in_current_span());
                        let pid = timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("inside the role transaction");
                        // Spend part of the original deadline before rollback; it must not get a fresh budget.
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                        proxy.arm(if fault == "rejected" { Fault::RejectedRollback } else { Fault::StalledRollback });
                        proxy.release();
                        timeout(Duration::from_secs(1), proxy.intercepted()).await.expect("rollback reached its injected boundary");
                        let error = timeout(Duration::from_millis(2300), task).await.expect("original three-second deadline").expect("read task").err().expect("rollback failure retains the determined rejection");
                        assert!(matches!(error, kernel::Error::Denied), "rollback failure preserves the original denial: {error:?}");
                        proxy.release();
                        timeout(Duration::from_secs(2), async {
                            while owner.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("backend observation").get::<_, bool>(0) { tokio::task::yield_now().await; }
                        }).await.expect("unconfirmed rollback ends its backend");
                    }
                    owner.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await; supplier.stop().await;
                }
            }).await.expect("denial and rollback scenarios");
        });
        logs_assert(|lines| {
            let failures: Vec<_> = lines
                .iter()
                .filter(|line| line.contains("rejection_rollback"))
                .collect();
            assert_eq!(failures.len(), 2, "each failed rollback is reported once");
            for line in &failures {
                assert!(
                    line.contains("rejection=") && line.contains("permission_denied"),
                    "{line}"
                );
                assert!(line.contains("stage: Read"), "{line}");
            }
            assert_eq!(
                failures
                    .iter()
                    .filter(|line| line.contains("kind: Query("))
                    .count(),
                1,
                "rejected rollback keeps its typed query failure"
            );
            assert_eq!(
                failures
                    .iter()
                    .filter(|line| line.contains("kind: Timeout("))
                    .count(),
                1,
                "stalled rollback keeps its typed deadline failure"
            );
            Ok(())
        });
    }

    #[test]
    fn role_list_deadline_ends_its_in_transaction_time_observation() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(12), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) =
                    start(authentication::proxied(&database, &proxy), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let owner = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("independent observer");
                proxy.arm(Fault::StalledTime);
                let service = services.role().clone();
                let task = tokio::spawn(async move {
                    service
                        .list(&actor, Pagination::new(1, 20).expect("page"))
                        .await
                });
                let pid = timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("in-transaction time observation");
                let error = timeout(Duration::from_secs(4), task)
                    .await
                    .expect("page deadline")
                    .expect("task")
                    .err()
                    .expect("no list after unconfirmed clock");
                let kernel::Error::Unavailable(failure) = &error else {
                    panic!("read deadline and later clock failure remain technical: {error:?}");
                };
                let timed_out = match failure.kind() {
                    kernel::FaultKind::Timeout(_)
                    | kernel::FaultKind::Clock(crate::runtime::Error::Timeout(_)) => true,
                    // The server's idle budget can end the transaction before the local deadline.
                    kernel::FaultKind::Clock(crate::runtime::Error::Query(error)) => error.code()
                        == Some(
                            &tokio_postgres::error::SqlState::IDLE_IN_TRANSACTION_SESSION_TIMEOUT,
                        ),
                    _ => false,
                };
                assert!(timed_out, "{error:?}");
                assert!(
                    !failure
                        .later()
                        .any(|(stage, cause)| stage == kernel::Stage::Time
                            && matches!(cause, kernel::FaultKind::Clock(_))),
                    "{error:?}"
                );
                proxy.release();
                services
                    .ready()
                    .await
                    .expect("ordinary observation timeout preserves admission");
                timeout(Duration::from_secs(2), async {
                    while owner
                        .client()
                        .query_one(
                            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",
                            &[&pid],
                        )
                        .await
                        .expect("page backend")
                        .get::<_, bool>(0)
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("cancelled page driver ended");
                owner.terminate().await.expect("observer ended");
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("context stopped");
                proxy.stop().await;
                supplier.stop().await;
            })
            .await
            .expect("time observation budget");
        });
    }

    #[test]
    fn role_names_and_protocol_categories_keep_their_exact_contract() {
        for value in ["普通账户", "系统管理员", "Á名", &"界".repeat(64)] {
            let name =
                Name::from_sql(&Type::TEXT, value.as_bytes()).expect("canonical stored name");
            assert_eq!(serde_json::to_value(&name).expect("name JSON"), value);
        }
        for value in ["", " bad ", "A\u{301}", "bad\0name", &"界".repeat(65)] {
            let error =
                Name::from_sql(&Type::TEXT, value.as_bytes()).expect_err("damaged canonical name");
            assert!(!format!("{error:?}").contains(&format!("\"{value}\"")));
        }
        let kinds = [Kind::Default, Kind::Full, Kind::Custom];
        assert_eq!(
            serde_json::to_value(kinds).expect("kind JSON"),
            serde_json::json!(["default", "full", "custom"])
        );
        assert_eq!(
            serde_json::to_value(Kind::schema()).expect("kind schema")["enum"],
            serde_json::json!(["default", "full", "custom"])
        );
        for code in Code::all() {
            let json = serde_json::to_value(code).expect("permission JSON");
            assert_eq!(json, code.as_ref());
            assert!(
                serde_json::to_value(Code::schema()).expect("permission schema")["enum"]
                    .as_array()
                    .expect("closed codes")
                    .contains(&json)
            );
        }
    }

    #[test]
    fn initialized_and_registered_accounts_read_their_complete_current_roles() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let id = message(&database, services.authentication(), &mut supplier, "13800138000").await;
                let Receipt::Registration(credential) = Box::pin(services.authentication().create_session(input(Uuid::new_v4(), id, "13800138000", CODE))).await.expect("R1 registration credential") else { panic!("unoccupied mobile"); };
                let issued = Box::pin(services.registration().register(crate::registration::Input::new(
                    Uuid::new_v4().to_string().parse().expect("operation"), credential.id(), credential.token().text().parse().expect("token"),
                    "Registered Operator".parse().expect("name"), "00000020000229007X".parse().expect("number"),
                ))).await.expect("actual R2");
                let ordinary = services.current().authenticate(Proof { id: issued.id(), token: issued.token().text().parse().expect("token") }).await.expect("registered actor");
                let observer = database.connections(&database.user).recover().await.expect("independent runtime observer");
                for (actor, level, kind, name, expected) in [
                    (&administrator, 1000, Kind::Full, "系统管理员", PERMISSIONS.as_slice()),
                    (&ordinary, 1, Kind::Default, "普通账户", &[][..]),
                ] {
                    let facts = observer.client().query_one("SELECT holder.role_id,role.change_id,appointment.sequence,holder.sequence AS holder_sequence,person.sequence,ARRAY(SELECT capability_id FROM access.permissions WHERE role_id=role.id ORDER BY capability_id COLLATE \"C\") AS permissions FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id JOIN access.holders holder ON holder.id=account.id JOIN access.appointments appointment ON appointment.id=holder.assignment_id JOIN access.roles role ON role.id=holder.role_id WHERE session.id=$1", &[&actor.session_id()]).await.expect("independent complete facts before read");
                    let role = services.role().read_current(actor).await.expect("current role");
                    assert_eq!(role.id(), facts.get("role_id"));
                    assert_eq!(role.change_id(), facts.get("change_id"));
                    assert_eq!((facts.get::<_, i64>("sequence"), facts.get::<_, i64>("holder_sequence")), (1, 1));
                    assert_eq!(facts.get::<_, i64>("sequence"), 1);
                    assert_eq!(facts.get::<_, Vec<String>>("permissions"), expected);
                    assert_eq!(role.permissions().iter().map(AsRef::as_ref).collect::<Vec<_>>(), expected);
                    assert_eq!((i16::from(role.level()), role.kind()), (level, kind));
                    assert_eq!(serde_json::to_value(role.name()).expect("role name JSON"), name);
                }
            for (actor, scope) in [
                (&administrator, crate::identity::revocation::Scope::Current),
                (&ordinary, crate::identity::revocation::Scope::All),
            ] {
                services.revocation().revoke(actor.clone(), Uuid::new_v4().to_string().parse().expect("logout operation"), scope).await.expect("actual logout after actor construction");
                assert!(observer.client().query_one("SELECT EXISTS (SELECT 1 FROM identity.revocations WHERE id=$1)", &[&actor.session_id()]).await.expect("independent revocation before role read").get::<_, bool>(0));
                assert!(matches!(services.role().read_current(actor).await, Err(kernel::Error::Unauthenticated)));
            }
            observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                supplier.stop().await;
            }).await.expect("real role sources budget");
        });
    }

    #[test]
    fn system_role_damage_stays_closed_after_repair_until_verified_restart() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["administrator name", "ordinary name", "ordinary time"] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let actor = if case == "administrator name" { manager.clone() } else { crate::access::appointment::tests::register(&database, &services, &mut supplier).await };
                    let role = services.role().read_current(&actor).await.expect("original system role");
                    let owner = database.connections("postgres").recover().await.expect("isolated damage injector");
                    let assigned_at: chrono::DateTime<chrono::Utc> = owner.client().query_one("SELECT assigned_at FROM access.appointments WHERE id=(SELECT assignment_id FROM access.holders WHERE id=$1)", &[&actor.account_id()]).await.expect("original assignment time").get(0);
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated system damage");
                    if case == "ordinary time" {
                        owner.client().execute("UPDATE access.appointments SET assigned_at=(SELECT changed_at FROM access.changes WHERE id=$1) WHERE id=(SELECT assignment_id FROM access.holders WHERE id=$2)", &[&role.change_id(), &actor.account_id()]).await.expect("rewrite the appointment time without resigning");
                    } else {
                        owner.client().execute("UPDATE access.roles SET name='private-canonical-name' WHERE id=$1", &[&role.id()]).await.expect("rename current system role");
                        owner.client().execute("UPDATE access.changes SET name='private-canonical-name' WHERE id=$1", &[&role.change_id()]).await.expect("rename paired system history");
                    }
                    owner.client().batch_execute("COMMIT").await.expect("visible damage");
                    let diagnostic = if case == "ordinary time" {
                        assert_eq!(services.role().read_current(&actor).await.expect("current authority uses the sealed Holder and Role states").id(), role.id());
                        let history = services.appointment().list(&manager, crate::access::appointment::Filter::new(Pagination::new(1, 100).expect("page"), None, None)).await;
                        let error = history.err().expect("appointment history rejects the rewritten record");
                        assert!(crate::runtime::work::Failure::unreliable(&error), "{error:?}");
                        format!("{error:?}")
                    } else {
                        let error = services.role().read_current(&actor).await.err().expect("damaged system definition withheld");
                        assert!(error.is_corrupt(), "{error:?}");
                        format!("{error:?}")
                    };
                    assert!(services.ready().await.is_err());
                    assert!(!diagnostic.contains("private-canonical-name"));
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("restore exact isolated facts");
                    owner.client().execute("UPDATE access.roles SET name=$1 WHERE id=$2", &[&role.name(), &role.id()]).await.expect("original system name");
                    owner.client().execute("UPDATE access.changes SET name=$1 WHERE id=$2", &[&role.name(), &role.change_id()]).await.expect("original historical name");
                    owner.client().execute("UPDATE access.appointments SET assigned_at=$1 WHERE id=(SELECT assignment_id FROM access.holders WHERE id=$2)", &[&assigned_at, &actor.account_id()]).await.expect("original assignment time");
                    owner.client().batch_execute("COMMIT").await.expect("facts repaired");
                    assert!(matches!(services.role().read_current(&actor).await, Err(error) if error.is_closed()), "repair does not reopen the original service");
                    owner.terminate().await.expect("owner ended");
                    services.drain().await.expect("closed services drained"); context.stop().await.expect("original context stopped");
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    assert_eq!(services.role().read_current(&actor).await.expect("restart verifies repaired facts").id(), role.id());
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("system role case {case}: {error}"));
            }
        }));
    }

    #[test]
    fn current_roles_use_sealed_state_and_history_reads_require_the_complete_chain() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for (case, damage) in [
                ("valid", ""),
                ("permissions", ""),
                ("live definition", ""),
                ("assignment before creation", ""),
                ("assignment equals creation", ""),
                ("assignment equals earlier change", ""),
                ("assignment equals latest change", ""),
                ("first update", "UPDATE access.changes SET action='update' WHERE role_id=$1 AND sequence=1"),
                ("after deletion", "UPDATE access.changes SET action='delete',permissions=NULL WHERE role_id=$1 AND sequence=2"),
                ("repeat create", "UPDATE access.changes SET action='create' WHERE role_id=$1 AND sequence=2"),
                ("equal early times", "UPDATE access.changes SET changed_at=(SELECT changed_at FROM access.changes WHERE role_id=$1 AND sequence=1) WHERE role_id=$1 AND sequence=2"),
                ("cross scope", "UPDATE access.changes SET scope='workforce',entry='institution' WHERE role_id=$1 AND sequence=2"),
                ("unchanged definition", "UPDATE access.changes SET name='First',level=200 WHERE role_id=$1 AND sequence=2"),
                ("system history", "UPDATE access.changes SET level=1000 WHERE role_id=$1 AND sequence=1"),
                ("missing predecessor", "DELETE FROM access.changes WHERE role_id=$1 AND sequence=2"),
                ("missing tail", "DELETE FROM access.changes WHERE role_id=$1 AND sequence=3"),
            ] {
                timeout(Duration::from_secs(40), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let reader = crate::access::appointment::tests::register(&database, &services, &mut supplier).await;
                    let input = |name: &str, level: i16, permissions: Vec<Code>| super::Input::new(
                        name.parse().expect("name"), level.try_into().expect("custom level"),
                        permissions.try_into().expect("scoped permissions"),
                    );
                    let permissions = case == "permissions";
                    let first = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("create operation"),
                        input(if permissions { "Custom role" } else { "First" }, 200,
                            if permissions { vec![Code::InstitutionRoles] } else { vec![] }))).await.expect("real creation");
                    let second = Box::pin(services.role().update(manager.clone(), Uuid::new_v4().to_string().parse().expect("first update operation"), first.id(),
                        super::Update::new(first.change_id(), input(if permissions { "Custom role" } else { "Second" }, if permissions { 200 } else { 201 },
                            if permissions { vec![Code::InstitutionAccounts] } else { vec![] })))).await.expect("real first modification");
                    let third = Box::pin(services.role().update(manager.clone(), Uuid::new_v4().to_string().parse().expect("second update operation"), first.id(),
                        super::Update::new(second.change_id(), input(if permissions { "Custom role" } else { "Third" }, if permissions { 200 } else { 202 },
                            if permissions { vec![Code::InstitutionAccounts, Code::InstitutionRoles] } else { vec![] })))).await.expect("real second modification");
                    let owner = database.connections("postgres").recover().await.expect("isolated history fault");
                    assign_role(owner.client(), services.role(), &reader, third.id()).await;
                    assert_eq!(services.role().read_current(&reader).await.expect("healthy current role").change_id(), third.change_id());
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated history damage");
                    if !damage.is_empty() {
                        assert_eq!(owner.client().execute(damage, &[&third.id()]).await.expect("one history mutation"), 1, "{case}");
                    }
                    if matches!(case, "live definition" | "assignment before creation" | "assignment equals creation" | "assignment equals earlier change" | "assignment equals latest change") {
                        let change_id = match case {
                            "live definition" | "assignment equals earlier change" => second.change_id(),
                            "assignment equals latest change" => third.change_id(),
                            _ => first.change_id(),
                        };
                        let statement = match case {
                            "live definition" => "SELECT earlier.changed_at + (later.changed_at-earlier.changed_at)/2 FROM access.changes earlier JOIN access.changes later ON later.id=$2 WHERE earlier.id=$1",
                            "assignment before creation" => "SELECT earlier.changed_at-interval '1 microsecond' FROM access.changes earlier JOIN access.changes later ON later.id=$2 WHERE earlier.id=$1",
                            _ => "SELECT earlier.changed_at FROM access.changes earlier JOIN access.changes later ON later.id=$2 WHERE earlier.id=$1",
                        };
                        let assigned_at: chrono::DateTime<chrono::Utc> = owner.client().query_one(statement, &[&change_id, &third.change_id()]).await.expect("explicit role event boundary").get(0);
                        owner.client().execute("UPDATE access.appointments SET assigned_at=$1 WHERE id=(SELECT assignment_id FROM access.holders WHERE id=$2)", &[&assigned_at, &reader.account_id()]).await.expect("exact assignment boundary");
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage visible independently");
                    let retained = if matches!(case, "missing predecessor" | "missing tail") { 2 } else { 3 };
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.changes WHERE role_id=$1", &[&third.id()]).await.expect("retained history count").get::<_, i64>(0), retained);
                    let current = services.role().read_current(&reader).await.expect("current state does not reconstruct either history chain");
                    assert_eq!(current.change_id(), third.change_id());
                    assert_eq!(current.kind(), Kind::Custom);
                    assert_eq!(current.permissions(), third.permissions());
                    assert_eq!(current.permissions().iter().map(AsRef::as_ref).collect::<Vec<_>>(), if permissions { vec!["institution.accounts", "institution.roles"] } else { vec![] });
                    let definitions = services.role().list(&manager, Pagination::new(1, 100).expect("definitions page")).await.expect("sealed definition list");
                    assert!(definitions.roles().iter().any(|role| role.id() == third.id() && role.change_id() == third.change_id()));
                    let filter = change::Filter::new(Pagination::new(1, 1).expect("one history item"), Some(third.id()), None);
                    let history = services.role().changes(&manager, filter).await;
                    if damage.is_empty() {
                        let history = history.expect("valid permanent role history");
                        assert_eq!(history.count(), 3);
                        assert_eq!(history.changes().len(), 1);
                        assert_eq!(history.changes()[0].id(), third.change_id());
                        if case == "live definition" || case.starts_with("assignment ") {
                            let history = services.appointment().list(&manager, crate::access::appointment::Filter::new(Pagination::new(1, 100).expect("appointment page"), None, None)).await;
                            let error = history.err().expect("appointment history rejects a record changed without its seal");
                            assert!(crate::runtime::work::Failure::unreliable(&error), "{case}: {error:?}");
                            assert!(services.ready().await.is_err(), "{case}: appointment history damage closes admission");
                        } else {
                            services.ready().await.expect("valid reads preserve admission");
                        }
                    } else {
                        let error = history.err().expect("damaged role history outside the page is rejected");
                        assert!(error.is_corrupt(), "{case}: {error:?}");
                        assert!(services.ready().await.is_err(), "{case}: role history damage closes admission");
                    }
                    owner.terminate().await.expect("fixture ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    supplier.stop().await;
                }).await.unwrap_or_else(|error| panic!("role history case {case}: {error}"));
            }
        });
    }

    #[test]
    fn decisive_identity_precedes_role_damage_and_required_relations_never_default() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for (damage, invalid, surface) in [
                    ("DELETE FROM access.holders", true, "current"),
                    ("DELETE FROM access.roles", true, "current"),
                    // Removing an anchored Session is damage, not an unknown credential.
                    ("DELETE FROM access.holders; DELETE FROM identity.sessions", false, "current"),
                    ("DELETE FROM access.holders; DELETE FROM identity.accounts", false, "current"),
                    ("DELETE FROM access.holders", false, "current"),
                    ("UPDATE access.holders SET person_id=gen_random_uuid()", false, "current"),
                    ("UPDATE access.holders SET role_id=(SELECT id FROM access.roles WHERE scope='workforce' AND level=1000)", false, "current"),
                    ("UPDATE access.holders SET assignment_id=gen_random_uuid()", false, "current"),
                    ("UPDATE identity.origins SET seal=set_byte(seal,0,get_byte(seal,0)#1)", false, "person"),
                    ("DELETE FROM access.roles", false, "current"),
                    ("DELETE FROM access.changes", false, "roles"),
                    ("UPDATE access.roles SET permissions=array_remove(permissions,'institution.roles') WHERE scope='institution' AND level=1000", false, "current"),
                    ("UPDATE access.roles SET permissions=ARRAY['workforce.roles'] WHERE scope='institution' AND level=1000", false, "current"),
                    ("UPDATE access.roles SET permissions=ARRAY['institution.roles','institution.roles'] WHERE scope='institution' AND level=1000", false, "current"),
                    ("UPDATE access.roles SET change_id=gen_random_uuid() WHERE scope='institution' AND level=1000", false, "current"),
                    ("UPDATE access.roles SET sequence=sequence+1 WHERE scope='institution' AND level=1000", false, "current"),
                    ("UPDATE access.roles SET version=version+1 WHERE scope='institution' AND level=1000", false, "current"),
                    ("UPDATE access.roles SET active=false WHERE scope='institution' AND level=1000", false, "current"),
                    ("UPDATE access.roles SET level=999 WHERE scope='institution' AND level=1000", false, "current"),
                    ("UPDATE access.roles SET changed_at=clock_timestamp()+interval '1 hour' WHERE scope='institution' AND level=1000", false, "current"),
                    ("UPDATE access.roles SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE scope='institution' AND level=1000", false, "current"),
                    ("UPDATE access.changes SET changed_at=changed_at+interval '1 hour'", false, "roles"),
                    ("UPDATE access.appointments SET assigned_at=clock_timestamp()+interval '1 hour'", false, "appointments"),
                    ("DELETE FROM access.appointments WHERE source='initialization'", false, "appointments"),
                    ("UPDATE access.appointments SET operation_id=gen_random_uuid()", false, "appointments"),
                    ("UPDATE access.changes SET operation_id=gen_random_uuid()", false, "roles"),
                    ("UPDATE access.holders SET sequence=sequence+1,version=version+1,changed_at=clock_timestamp()", false, "current"),
                    ("ALTER TABLE access.roles DROP CONSTRAINT roles_name_check; ALTER TABLE access.changes DROP CONSTRAINT changes_name_check; UPDATE access.roles SET name=' private-role-value ' WHERE scope='institution' AND level=1000; UPDATE access.changes SET name=' private-role-value ' WHERE scope='institution' AND level=1000", false, "current"),
                    ("UPDATE identity.sessions SET app_id='private-foreign-app'", false, "current"),
            ] {
                timeout(Duration::from_secs(60), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let administrator = actor(&database, &services, &mut supplier).await;
                    let suspended=invalid && damage=="DELETE FROM access.roles";
                    let actor=if suspended { crate::access::appointment::tests::register(&database,&services,&mut supplier).await } else { administrator.clone() };
                    services.role().read_current(&actor).await.expect("healthy precondition");
                    if suspended {
                        let candidate=services.account().candidate(&administrator,"13800000000".parse().expect("mobile")).await.expect("actual lower target");
                        services.suspension().suspend(administrator,Uuid::new_v4().to_string().parse().expect("pause operation"),candidate.identity().id(),serde_json::from_value(serde_json::json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"})).expect("pause confirmation")).await.expect("real Account suspension before role damage");
                    } else if invalid && damage=="DELETE FROM access.holders" {
                        let observer=database.connections("postgres").recover().await.expect("fixture lifetime observer");
                        let expires_at:chrono::DateTime<chrono::Utc>=observer.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1",&[&actor.session_id()]).await.expect("original session lifetime").get(0);
                        observer.terminate().await.expect("observer ended");
                        crate::registration::tests::age_facts(&database,&services,expires_at-database.now().await+TimeDelta::seconds(1)).await;
                    }
                    // This disposable corruption case also removes row checks that replica mode preserves.
                    let owner = database.connections("postgres").recover().await.expect("isolated damage injector");
                    owner.client().batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; {damage}; COMMIT")).await.expect("isolated state or damage after actor construction");
                    if surface != "current" {
                        let current = services.role().read_current(&actor).await.expect("current authority does not scan permanent history");
                        assert_eq!(current.kind(), Kind::Full);
                        services.ready().await.expect("current state is still authenticated");
                    }
                    let diagnostic = match surface {
                        "current" => {
                            let error = services.role().read_current(&actor).await.err().expect("changed state rejects projection");
                            assert_eq!(matches!(error, kernel::Error::Unauthenticated), invalid, "{damage}: {error:?}");
                            if !invalid { assert!(error.is_corrupt(), "{damage}: {error:?}"); }
                            format!("{error:?}")
                        }
                        "roles" => {
                            let history = services.role().changes(&actor, change::Filter::new(Pagination::new(1, 100).expect("page"), None, None)).await;
                            let error = history.err().expect("role history rejects the damaged permanent records");
                            assert!(error.is_corrupt(), "{damage}: {error:?}");
                            format!("{error:?}")
                        }
                        "appointments" => {
                            let history = services.appointment().list(&actor, crate::access::appointment::Filter::new(Pagination::new(1, 100).expect("page"), None, None)).await;
                            let error = history.err().expect("appointment history rejects the damaged permanent records");
                            assert!(crate::runtime::work::Failure::unreliable(&error), "{damage}: {error:?}");
                            format!("{error:?}")
                        }
                        "person" => {
                            let error = crate::identity::person::verify(owner.client(), services.app().keys(), chrono::Utc::now()).await.expect_err("startup rejects an incomplete person record family");
                            format!("{error:?}")
                        }
                        _ => unreachable!("closed read surfaces"),
                    };
                    assert_eq!(services.ready().await.is_ok(), invalid || surface == "person", "only authentication rejection or standalone startup verification preserves admission: {damage}");
                    assert!(!diagnostic.contains("private-role-value"));
                    assert!(!diagnostic.contains("private-foreign-app"));
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    supplier.stop().await;
                }).await.unwrap_or_else(|error| panic!("role identity case {damage}: {error}"));
            }
        });
    }

    #[test]
    fn new_role_reads_use_fresh_time_and_preserve_sampling_after_cancellation() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                for fault in ["expiry", "rewind", "cancel"] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let owner = database.connections("postgres").recover().await.expect("observer");
                    let expires_at:chrono::DateTime<chrono::Utc>=owner.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1",&[&actor.session_id()]).await.expect("original session lifetime").get(0);
                    if matches!(fault, "expiry" | "rewind") {
                        let first = context.connections().lease().await.expect("first lease");
                        let second = context.connections().lease().await.expect("second lease");
                        let service = services.role().clone();
                        let mut reader = tokio::spawn(async move { service.read_current(&actor).await });
                        assert!(timeout(Duration::from_millis(30), &mut reader).await.is_err(), "reader waits for a pooled connection before sampling");
                        assert!(!reader.is_finished());
                        if fault == "rewind" {
                            proxy.arm(Fault::RewoundCurrent); first.release(); second.release();
                            timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual decisive query");
                            proxy.release();
                        } else {
                            crate::registration::tests::age_facts(&database,&services,expires_at-database.now().await+TimeDelta::seconds(1)).await;
                            first.release(); second.release();
                        }
                        let error = timeout(Duration::from_secs(3), reader).await.expect("reader finished").expect("reader task").err().expect("no stale role");
                        if fault == "rewind" { assert!(matches!(&error, kernel::Error::Unavailable(fault) if matches!(fault.kind(), kernel::FaultKind::Clock(crate::runtime::Error::ClockInvalid)))); assert!(services.ready().await.is_err()); }
                        else { assert!(matches!(error, kernel::Error::Unauthenticated)); }
                    } else {
                        proxy.arm(Fault::StalledCurrent);
                        let service = services.role().clone();
                        let reader_actor = actor.clone();
                        let reader = tokio::spawn(async move { service.read_current(&reader_actor).await });
                        timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("decisive query in the snapshot");
                        reader.abort(); assert!(reader.await.is_err_and(|error| error.is_cancelled()));
                        proxy.release();
                        services.role().read_current(&actor).await.expect("a fresh role read");
                        services.ready().await.expect("caller cancellation preserves admission");
                    }
                    owner.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); let _ = context.stop().await;
                    proxy.stop().await; supplier.stop().await;
                }
            }).await.expect("role clock boundaries");
        });
    }
}
