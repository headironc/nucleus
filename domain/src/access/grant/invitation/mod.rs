//! Workforce invitation codes created under current domain admission authority.

pub mod acceptance;
pub mod adjustment;
mod code;
mod command;
mod database;
pub mod keys;
mod record;
mod state;
mod transition;
mod verification;

#[cfg(test)]
mod integrity_tests;
pub mod withdrawal;

use bytes::BytesMut;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::{AsRefStr, EnumString};
use tokio::time::timeout_at;
use tokio_postgres::{
    Client,
    types::{FromSql, ToSql, Type},
};
use utoipa::ToSchema;
use uuid::Uuid;

pub use crate::access::grant::invitation::code::Code;
use crate::{
    access::{
        grant::{Error, Service, member},
        role,
    },
    identity::{current::Actor, operation},
};

/// Creating a code never repeats a stored lookup digest under any retained key.
const ATTEMPTS: usize = 3;

pub(in crate::access) use database::in_use;
pub(crate) use record::{ACCEPTANCES, ADJUSTMENTS, CREATIONS, TRANSITIONS, WITHDRAWALS};
pub(crate) use state::TABLE;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = invitation::Id, value_type = Uuid)]
pub struct Id(Uuid);

impl Id {
    fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(in crate::access::grant) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

/// The positive cumulative number of successful acceptances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(try_from = "i64", into = "i64")]
#[schema(as = invitation::Capacity, value_type = i64, minimum = 1)]
pub struct Capacity(i64);

impl TryFrom<i64> for Capacity {
    type Error = CapacityError;

    fn try_from(value: i64) -> Result<Self, CapacityError> {
        if value < 1 {
            return Err(CapacityError);
        }
        Ok(Self(value))
    }
}

impl From<Capacity> for i64 {
    fn from(capacity: Capacity) -> Self {
        capacity.0
    }
}

impl ToSql for Capacity {
    fn to_sql(
        &self,
        kind: &Type,
        output: &mut BytesMut,
    ) -> Result<tokio_postgres::types::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.0.to_sql(kind, output)
    }

    fn accepts(kind: &Type) -> bool {
        <i64 as ToSql>::accepts(kind)
    }

    tokio_postgres::types::to_sql_checked!();
}

impl<'a> FromSql<'a> for Capacity {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self::try_from(i64::from_sql(kind, raw)?)?)
    }

    fn accepts(kind: &Type) -> bool {
        <i64 as FromSql>::accepts(kind)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invitation capacity must be a positive integer")]
pub struct CapacityError;

/// A confirmed expiry that PostgreSQL stores exactly, so replay compares the original value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(try_from = "DateTime<Utc>", into = "DateTime<Utc>")]
#[schema(as = invitation::Expiry, value_type = String, format = DateTime)]
pub struct Expiry(DateTime<Utc>);

impl TryFrom<DateTime<Utc>> for Expiry {
    type Error = ExpiryError;

    fn try_from(value: DateTime<Utc>) -> Result<Self, ExpiryError> {
        if !value.timestamp_subsec_nanos().is_multiple_of(1_000) {
            return Err(ExpiryError);
        }
        Ok(Self(value))
    }
}

impl From<Expiry> for DateTime<Utc> {
    fn from(expiry: Expiry) -> Self {
        expiry.0
    }
}

impl ToSql for Expiry {
    fn to_sql(
        &self,
        kind: &Type,
        output: &mut BytesMut,
    ) -> Result<tokio_postgres::types::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.0.to_sql(kind, output)
    }

    fn accepts(kind: &Type) -> bool {
        <DateTime<Utc> as ToSql>::accepts(kind)
    }

    tokio_postgres::types::to_sql_checked!();
}

impl<'a> FromSql<'a> for Expiry {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self::try_from(DateTime::<Utc>::from_sql(kind, raw)?)?)
    }

    fn accepts(kind: &Type) -> bool {
        <DateTime<Utc> as FromSql>::accepts(kind)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invitation expiry must not be finer than a microsecond")]
pub struct ExpiryError;

/// A currently established reason why the code cannot be accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema, AsRefStr, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[schema(as = invitation::Reason)]
pub enum Reason {
    Expired,
    Withdrawn,
    GrantTerminated,
    AccountCancelled,
    AdmissionLost,
    AccountSuspended,
    GrantSuspended,
    RoleUnassignable,
    CapacityReached,
}

impl<'a> FromSql<'a> for Reason {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(<&str>::from_sql(kind, raw)?.parse()?)
    }

    fn accepts(kind: &Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = invitation::Input)]
pub struct Input {
    role: role::Selection,
    capacity: Capacity,
    expires_at: Expiry,
}

pub struct Invitation {
    id: Id,
    role_id: role::Id,
    capacity: Capacity,
    expires_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    operation_id: operation::Id,
    reasons: Vec<Reason>,
    code: Option<Code>,
}

impl Invitation {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn role_id(&self) -> role::Id {
        self.role_id
    }
    pub fn capacity(&self) -> Capacity {
        self.capacity
    }
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
    pub fn reasons(&self) -> &[Reason] {
        &self.reasons
    }
    /// Present only while the original creator may still copy a usable code.
    pub fn code(&self) -> Option<&Code> {
        self.code.as_ref()
    }
}

/// Retained history is checked in bounded batches without a total scan deadline.
pub(crate) async fn verify(
    client: &Client,
    roles: &role::Reader,
    keys: &keys::Keys,
    revisions: &crate::identity::person::revision::Reader,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    Ok(verification::verify(client, roles, keys, revisions, now).await?)
}

impl Service {
    pub async fn create_invitation(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<Invitation, Error> {
        command::create(self, actor, operation_id, input).await
    }

    /// The original creator reads current reasons, and the code only while it may be copied.
    pub async fn read_invitation(&self, actor: &Actor, id: Id) -> Result<Invitation, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    async move {
                        let (_, person_id, _, now) = member::authorize(
                            client,
                            &roles,
                            &self.current,
                            actor,
                            role::Code::WorkforceAdmission,
                        )
                        .await?;
                        let fact = database::find_by_creator(
                            client,
                            &roles,
                            id,
                            person_id,
                            roles.read()?,
                            now,
                        )
                        .await?
                        .ok_or(Error::Absent)?;
                        fact.deliver(&self.invitations)
                    }
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }
}

#[cfg(test)]
pub(super) mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use chrono::{DateTime, Utc};
    use serde_json::{Value, json};
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use super::{Input, Invitation, Reason};
    use crate::{
        access::{
            Entry,
            appointment::tests::register,
            grant::{Error, tests::admit},
            role::{
                self, Kind,
                tests::{actor, start},
            },
        },
        identity::{
            authentication::{Database, Supplier, server},
            current::Actor,
            operation,
        },
        pagination::Pagination,
        startup::Services,
    };

    pub(super) async fn state(
        client: &tokio_postgres::Client,
        services: &Services,
        id: super::Id,
    ) -> crate::history::Verified<super::state::State> {
        crate::history::find::<super::state::State>(
            client,
            services.app().keys(),
            services.app().anchors(),
            crate::history::Read::Locked,
            id,
            Utc::now(),
        )
        .await
        .expect("authenticated invitation")
        .expect("invitation")
    }

    pub(super) fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    /// A selected Workforce definition as the client confirms it.
    pub(super) struct Choice {
        id: role::Id,
        change_id: role::change::Id,
    }

    impl Choice {
        pub(super) fn id(&self) -> role::Id {
            self.id
        }
        pub(super) fn change_id(&self) -> role::change::Id {
            self.change_id
        }
    }

    impl From<&role::Role> for Choice {
        fn from(role: &role::Role) -> Self {
            Self {
                id: role.id(),
                change_id: role.change_id(),
            }
        }
    }

    pub(super) fn body(role: &Choice, capacity: i64, expires_at: DateTime<Utc>) -> Value {
        json!({"role":{"id":role.id(),"changeId":role.change_id()},"capacity":capacity,"expiresAt":expires_at})
    }

    pub(super) fn input(value: &Value) -> Input {
        serde_json::from_value(value.clone()).expect("typed invitation")
    }

    /// Client expiries carry at most microsecond precision.
    pub(super) fn later() -> DateTime<Utc> {
        chrono::SubsecRound::trunc_subsecs(Utc::now() + chrono::Duration::days(7), 6)
    }

    pub(super) fn text(invitation: &Invitation) -> String {
        serde_json::to_value(invitation.code().expect("copyable original code"))
            .expect("code text")
            .as_str()
            .expect("string code")
            .to_owned()
    }

    pub(super) async fn workforce_role(services: &Services, actor: &Actor, kind: Kind) -> Choice {
        services
            .role()
            .list_workforce(actor, Pagination::new(1, 100).expect("page"))
            .await
            .expect("current Workforce definitions")
            .roles()
            .iter()
            .find(|role| role.kind() == kind)
            .map(Choice::from)
            .expect("system Workforce role")
    }

    /// An institution administrator and a registered Workforce administrator admitted by it.
    pub(super) async fn creator(
        database: &Database,
        services: &Services,
        supplier: &mut Supplier,
    ) -> (Actor, Actor) {
        let institution = actor(database, services, supplier).await;
        let member = Box::pin(register(database, services, supplier)).await;
        Box::pin(admit(
            services,
            &institution,
            Entry::Institution,
            "13800000000",
            Kind::Full,
        ))
        .await;
        (institution, member)
    }

    /// Read authenticated current reasons through an independent database connection.
    pub(super) async fn reasons(
        observer: &platform::database::Session,
        services: &Services,
        id: super::Id,
    ) -> Vec<String> {
        let service = services.grant();
        let now = service
            .current
            .now(observer.client())
            .await
            .expect("trusted time");
        super::database::find_by_id(observer.client(), &service.roles, id, now)
            .await
            .expect("authenticated independent read")
            .expect("retained invitation")
            .deliver(&service.invitations)
            .expect("authenticated code")
            .reasons()
            .iter()
            .map(|reason| reason.as_ref().to_owned())
            .collect()
    }

    #[test]
    fn h2_tombstone_cancelled_creator_reasons_drop_the_former_account_suspension() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, member) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let selected = workforce_role(&services, &member, Kind::Default).await;
                let created = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&selected, 3, later())))).await.expect("real creator invitation");
                let observer = database.connections(&database.user).recover().await.expect("runtime observer");
                assert!(reasons(&observer, &services, created.id()).await.is_empty());
                let candidate = services.account().candidate(&institution, "13800000000".parse().expect("creator mobile")).await.expect("creator confirmation");
                let identity = candidate.identity();
                let confirmation = json!({"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13800000000"});
                Box::pin(services.suspension().suspend(institution.clone(), operation(), identity.id(), serde_json::from_value(confirmation.clone()).expect("suspension confirmation"))).await.expect("real creator account suspension");
                assert_eq!(reasons(&observer, &services, created.id()).await, ["account_suspended"]);
                Box::pin(services.cancellation().cancel_account(institution, operation(), identity.id(), serde_json::from_value(confirmation).expect("managed cancellation confirmation"))).await.expect("real management cancellation of the paused creator");
                assert!(observer.client().query_one("SELECT account.cancellation_id IS NOT NULL AND EXISTS(SELECT 1 FROM identity.suspensions suspension WHERE suspension.account_id=account.id AND NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE id=suspension.id)) FROM identity.accounts account WHERE account.id=$1", &[&identity.id()]).await.expect("tombstone retains its historical suspension").get::<_, bool>(0));
                assert_eq!(reasons(&observer, &services, created.id()).await, ["account_cancelled"]);
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
            })).await.expect("bounded cancelled creator reasons regression");
        }));
    }

    #[test]
    fn capacity_accepts_only_positive_integers() {
        for value in [json!(0), json!(-1), json!(1.5), json!("1"), Value::Null] {
            assert!(serde_json::from_value::<super::Capacity>(value).is_err());
        }
        assert_eq!(
            i64::from(
                serde_json::from_value::<super::Capacity>(json!(i64::MAX)).expect("bigint maximum")
            ),
            i64::MAX
        );
    }

    #[test]
    fn creation_saves_one_permanent_code_under_current_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, member) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let default = workforce_role(&services, &member, Kind::Default).await;
                let full = workforce_role(&services, &member, Kind::Full).await;
                let expires_at = later();
                let original = operation();
                let value = body(&default, 3, expires_at);
                let created = Box::pin(services.grant().create_invitation(member.clone(), original, input(&value))).await.expect("current admission creates an invitation");
                assert_eq!(created.role_id(), default.id());
                assert_eq!(i64::from(created.capacity()), 3);
                assert_eq!(created.expires_at(), expires_at);
                assert_eq!(created.operation_id(), original);
                assert!(created.reasons().is_empty());
                let code = text(&created);
                assert_eq!(code.len(), 43);
                assert!(code.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let grant = services.grant().read_latest(&member).await.expect("creator grant");
                let current = services.current().read(&member).await.expect("creator identity");
                let row = observer.client().query_one("SELECT person_id, grant_id, role_id, operator_id, capacity, expires_at, created_at, key_id, octet_length(code) AS sealed, octet_length(hash) AS digest, domain FROM access.invitations WHERE id = $1 AND operation_id = $2", &[&created.id(), &original]).await.expect("one permanent invitation before replay");
                assert_eq!(row.get::<_, crate::identity::person::Id>("person_id"), current.person_id());
                assert_eq!(row.get::<_, crate::access::grant::Id>("grant_id"), grant.id());
                assert_eq!(row.get::<_, role::Id>("role_id"), default.id());
                assert_eq!(row.get::<_, crate::identity::person::revision::Id>("operator_id"), current.revision_id());
                assert_eq!(row.get::<_, i64>("capacity"), 3);
                assert_eq!(row.get::<_, DateTime<Utc>>("expires_at"), expires_at);
                assert_eq!(row.get::<_, DateTime<Utc>>("created_at"), created.created_at());
                assert_eq!(row.get::<_, i64>("key_id"), 0);
                assert_eq!(row.get::<_, i32>("sealed"), 69);
                assert_eq!(row.get::<_, i32>("digest"), 32);
                assert_eq!(row.get::<_, &str>("domain"), "workforce");
                let replay = Box::pin(services.grant().create_invitation(member.clone(), original, input(&value))).await.expect("complete original input replays");
                assert_eq!(replay.id(), created.id());
                assert_eq!(text(&replay), code);
                for changed in [body(&default, 4, expires_at), body(&default, 3, expires_at + chrono::Duration::seconds(1)), body(&full, 3, expires_at)] {
                    assert!(matches!(Box::pin(services.grant().create_invitation(member.clone(), original, input(&changed))).await, Err(Error::Mismatch)));
                }
                let stale = json!({"role":{"id":default.id(),"changeId":full.change_id()},"capacity":1,"expiresAt":expires_at});
                assert!(matches!(Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&stale))).await, Err(Error::RoleChanged)));
                assert!(matches!(Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&full, 1, expires_at)))).await, Err(Error::Denied)), "the creator's own level is not assignable");
                assert!(matches!(Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&default, 1, chrono::SubsecRound::trunc_subsecs(Utc::now() - chrono::Duration::seconds(1), 6))))).await, Err(Error::Expiry)));
                assert!(matches!(Box::pin(services.grant().create_invitation(institution.clone(), operation(), input(&value))).await, Err(Error::Denied)), "institutional authority without membership cannot create");
                let other = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&value))).await.expect("a second operation creates another code");
                assert_ne!(text(&other), code);
                assert_eq!(observer.client().query_one("SELECT count(*), count(DISTINCT hash) FROM access.invitations", &[]).await.expect("permanent facts after rejections").get::<_, i64>(0), 2);
                services.ready().await.expect("business rejections preserve admission");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let restarted = Box::pin(services.grant().create_invitation(member, original, input(&value))).await.expect("restart verifies and replays the retained code");
                assert_eq!(text(&restarted), code);
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded invitation creation");
        }));
    }

    #[test]
    fn usable_invitations_keep_their_role_and_copy_only_while_available() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let member = Box::pin(register(&database, &services, &mut supplier)).await;
                let inviter = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Inviter","level":500,"permissions":["workforce.admission"]})).expect("inviter definition"))).await.expect("custom admission role");
                let invited = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Invited","level":100,"permissions":[]})).expect("invited definition"))).await.expect("custom selected role");
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Custom)).await;
                assert_eq!(services.grant().read_role(&member).await.expect("current role").id(), inviter.id());
                let original = operation();
                let value = body(&Choice::from(&invited), 2, later());
                let created = Box::pin(services.grant().create_invitation(member.clone(), original, input(&value))).await.expect("custom admission authority creates");
                let delete = |role: &role::Role| serde_json::from_value::<role::workforce::Delete>(json!({"entry":"institution","changeId":role.change_id()})).expect("deletion");
                assert!(matches!(Box::pin(services.role().delete_workforce(institution.clone(), operation(), invited.id(), delete(&invited))).await, Err(crate::kernel::Error::Rejected(role::Rejection::InUse))), "a usable invitation keeps its selected role");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                assert!(reasons(&observer, &services, created.id()).await.is_empty());
                let removed = Box::pin(services.role().update_workforce(institution.clone(), operation(), inviter.id(), serde_json::from_value(json!({"entry":"institution","changeId":inviter.change_id(),"name":"Inviter","level":500,"permissions":[]})).expect("removed authority"))).await.expect("definition loses admission");
                assert_eq!(reasons(&observer, &services, created.id()).await, ["admission_lost"]);
                assert!(matches!(Box::pin(services.grant().create_invitation(member.clone(), original, input(&value))).await, Err(Error::Denied)), "replay requires current basic authority");
                let restored = Box::pin(services.role().update_workforce(institution.clone(), operation(), inviter.id(), serde_json::from_value(json!({"entry":"institution","changeId":removed.change_id(),"name":"Inviter","level":500,"permissions":["workforce.admission"]})).expect("restored authority"))).await.expect("definition regains admission");
                let replay = Box::pin(services.grant().create_invitation(member.clone(), original, input(&value))).await.expect("original result after regaining authority");
                assert_eq!(replay.id(), created.id());
                assert_eq!(replay.reasons(), [Reason::AdmissionLost]);
                assert!(replay.code().is_none(), "regaining authority does not revive a permanently unusable code");
                Box::pin(services.role().delete_workforce(institution.clone(), operation(), invited.id(), delete(&invited))).await.expect("a permanently unusable invitation no longer keeps the role");
                assert!(observer.client().query_one("SELECT EXISTS (SELECT 1 FROM access.roles WHERE id = $1 AND NOT active AND permissions=ARRAY[]::text[]) AND EXISTS (SELECT 1 FROM access.invitations WHERE id = $2 AND role_id = $1)", &[&invited.id(), &created.id()]).await.expect("permanent role tombstone retains the invitation").get::<_, bool>(0));
                let current = workforce_role(&services, &institution, Kind::Default).await;
                let ended = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&current, 1, later())))).await.expect("a new code after regaining authority");
                let grant = services.grant().read_latest(&member).await.expect("creator grant");
                Box::pin(services.grant().terminate(member.clone(), operation(), grant.id())).await.expect("creator exits");
                assert_eq!(reasons(&observer, &services, ended.id()).await, ["grant_terminated"]);
                Box::pin(services.role().update_workforce(institution.clone(), operation(), inviter.id(), serde_json::from_value(json!({"entry":"institution","changeId":restored.change_id(),"name":"Inviter","level":500,"permissions":[]})).expect("removal after exit"))).await.expect("former role loses admission after the grant ended");
                assert_eq!(reasons(&observer, &services, ended.id()).await, ["grant_terminated"], "a role change after the holding period ended is not admission loss");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded invitation availability");
        }));
    }

    #[test]
    fn creation_recovers_its_original_result_across_commit_faults() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["lost", "rollback", "unknown", "cancelled"] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let (_, member) = Box::pin(creator(&database, &services, &mut supplier)).await;
                    let default = workforce_role(&services, &member, Kind::Default).await;
                    let value = body(&default, 1, later());
                    let operation_id = operation();
                    proxy.arm(match case { "lost" => Fault::LostCommit, "rollback" => Fault::RolledBack, "unknown" => Fault::UnknownCommit, "cancelled" => Fault::DelayedCommit, _ => unreachable!() });
                    let service = services.grant().clone(); let actor = member.clone(); let original = input(&value);
                    let waiter = tokio::spawn(async move { service.create_invitation(actor, operation_id, original).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original commit intercepted");
                    if case == "cancelled" {
                        waiter.abort(); assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                        services.close();
                        assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "a cancelled wait keeps accepted work until completion");
                        proxy.release(); services.drain().await.expect("accepted work drained");
                    } else {
                        proxy.release();
                        let result = timeout(Duration::from_secs(15), waiter).await.expect("bounded outcome").expect("waiter");
                        match case {
                            "rollback" => assert!(matches!(result, Err(Error::Command(crate::kernel::Error::Unavailable(ref fault))) if fault.stage() == crate::kernel::Stage::Commit)),
                            "unknown" => assert!(matches!(result, Err(Error::Command(crate::kernel::Error::Unknown(_))))),
                            _ => assert!(result.expect("recovered original").code().is_some()),
                        }
                        services.close(); services.drain().await.expect("drained");
                    }
                    context.stop().await.expect("stopped");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    // An unknown outcome concludes once the recovery BEGIN fails; release only lets the
                    // proxy forward the original COMMIT. Cross the actor's account lock before reading.
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT id FROM identity.accounts WHERE id = $1 FOR UPDATE", &[&member.account_id()]).await.expect("original backend released the actor's account lock");
                    let committed = i64::from(case != "rollback");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.invitations WHERE operation_id = $1", &[&operation_id]).await.expect("durable state before replay").get::<_, i64>(0), committed);
                    observer.client().batch_execute("COMMIT").await.expect("observation complete");
                    proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let replay = Box::pin(services.grant().create_invitation(member, operation_id, input(&value))).await.expect("original result or explicit retry after proven rollback");
                    assert!(replay.code().is_some());
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.invitations", &[]).await.expect("one invitation after replay").get::<_, i64>(0), 1);
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded recovery case");
            }
        }));
    }

    #[test]
    fn runtime_sql_requires_paired_state_advancement_and_permanent_creation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (_, member) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let default = workforce_role(&services, &member, Kind::Default).await;
                let created = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&default, 1, later())))).await.expect("valid invitation");
                let runtime = database.connections(&database.user).recover().await.expect("runtime identity");
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                for statement in [
                    "UPDATE access.invitations SET current_capacity = current_capacity + 1",
                    "UPDATE access.invitations SET capacity = capacity + 1",
                    "UPDATE access.invitations SET version = version + 1, sequence = sequence + 1",
                ] {
                    let error = runtime.client().batch_execute(statement).await.expect_err("state requires a matching new record");
                    assert_eq!(error.as_db_error().expect("constraint").constraint(), Some("invitations_state_check"));
                }
                assert_eq!(runtime.client().batch_execute("DELETE FROM access.invitations").await.expect_err("runtime cannot delete state").code().expect("privilege").code(), "42501");
                assert_eq!(owner.client().batch_execute("DELETE FROM access.invitations").await.expect_err("owner cannot delete state").code().expect("constraint").code(), "23514");
                for statement in ["UPDATE access.creations SET capacity = capacity", "DELETE FROM access.creations"] {
                    assert_eq!(runtime.client().batch_execute(statement).await.expect_err("runtime cannot mutate creation").code().expect("privilege").code(), "42501");
                    assert_eq!(owner.client().batch_execute(statement).await.expect_err("creation is permanent").code().expect("constraint").code(), "23514");
                }
                assert_eq!(runtime.client().query_one("SELECT count(*) FROM access.invitations WHERE id=$1", &[&created.id()]).await.expect("unchanged").get::<_, i64>(0), 1);
                runtime.terminate().await.expect("runtime ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                for case in ["ciphertext", "digest"] {
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                    let statement = match case { "ciphertext" => "UPDATE access.invitations SET code = set_byte(code, 68, get_byte(code, 68) # 1)", _ => "UPDATE access.invitations SET hash = set_byte(hash, 0, get_byte(hash, 0) # 1)" };
                    owner.client().batch_execute(statement).await.expect("damage");
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                    let result = Box::pin(Services::start(
                        crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                        crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                        crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"),
                        supplier.client(), crate::identity::authentication::unused_client().await,
                    )).await;
                    assert!(matches!(result, Err(crate::startup::Error::Grant(Error::Command(crate::kernel::Error::Corrupt(_))))), "startup rejects damaged sealed invitation: {:?}", result.as_ref().err());
                    context.stop().await.expect("failed startup context stopped");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated repair");
                    let repair = match case { "ciphertext" => "UPDATE access.invitations SET code = set_byte(code, 68, get_byte(code, 68) # 1)", _ => "UPDATE access.invitations SET hash = set_byte(hash, 0, get_byte(hash, 0) # 1)" };
                    owner.client().batch_execute(repair).await.expect("repair");
                    owner.client().batch_execute("COMMIT").await.expect("repair committed");
                }
                owner.terminate().await.expect("owner ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded database protection");
        }));
    }

    #[test]
    fn startup_verifies_every_retained_invitation_across_batches() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(90), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (_, member) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let default = workforce_role(&services, &member, Kind::Default).await;
                for _ in 0..130 {
                    Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&default, 1, later())))).await.expect("retained invitation");
                }
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("restart verifies more than one batch");
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                // The greatest UUID is read only by the second batch.
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE access.invitations SET hash = set_byte(hash, 0, get_byte(hash, 0) # 1) WHERE id = (SELECT id FROM access.invitations ORDER BY id DESC LIMIT 1); COMMIT").await.expect("damage the last batch");
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result = Box::pin(Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                    crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                    crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(), crate::identity::authentication::unused_client().await,
                )).await;
                assert!(matches!(result, Err(crate::startup::Error::Grant(Error::Command(crate::kernel::Error::Corrupt(_))))), "damage after the first batch still blocks startup: {:?}", result.as_ref().err());
                context.stop().await.expect("failed startup context stopped");
                owner.terminate().await.expect("owner ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded batched verification");
        }));
    }

    #[test]
    fn only_the_current_creator_reads_reasons_and_a_copyable_code() {
        use crate::access::appointment::tests::{mobile, register_numbered};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let member = Box::pin(register(&database, &services, &mut supplier)).await;
                let inviter = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Inviter","level":500,"permissions":["workforce.admission"]})).expect("inviter definition"))).await.expect("custom admission role");
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Custom)).await;
                let peer = Box::pin(register_numbered(&database, &services, &mut supplier, 10)).await;
                Box::pin(admit(&services, &institution, Entry::Institution, &mobile(10), Kind::Full)).await;
                let default = workforce_role(&services, &institution, Kind::Default).await;
                let created = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&default, 2, later())))).await.expect("creator invitation");
                let read = services.grant().read_invitation(&member, created.id()).await.expect("creator reads the original");
                assert_eq!((read.id(), read.role_id(), read.capacity(), read.expires_at(), read.created_at(), read.operation_id()), (created.id(), created.role_id(), created.capacity(), created.expires_at(), created.created_at(), created.operation_id()));
                assert!(read.reasons().is_empty());
                assert_eq!(text(&read), text(&created));
                assert!(matches!(services.grant().read_invitation(&member, super::Id::allocate()).await, Err(Error::Absent)));
                assert!(matches!(services.grant().read_invitation(&institution, created.id()).await, Err(Error::Denied)), "institutional authority is not a member");
                let removed = Box::pin(services.role().update_workforce(institution.clone(), operation(), inviter.id(), serde_json::from_value(json!({"entry":"institution","changeId":inviter.change_id(),"name":"Inviter","level":500,"permissions":[]})).expect("removed authority"))).await.expect("creator loses admission");
                assert!(matches!(services.grant().read_invitation(&member, created.id()).await, Err(Error::Denied)), "reading requires current admission authority");
                Box::pin(services.role().update_workforce(institution.clone(), operation(), inviter.id(), serde_json::from_value(json!({"entry":"institution","changeId":removed.change_id(),"name":"Inviter","level":500,"permissions":["workforce.admission"]})).expect("restored authority"))).await.expect("creator regains admission");
                let lost = services.grant().read_invitation(&member, created.id()).await.expect("regained authority reads again");
                assert_eq!(lost.reasons(), [Reason::AdmissionLost]);
                assert!(lost.code().is_none(), "a permanently unusable code is no longer copied");
                let other = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&default, 1, later())))).await.expect("a usable code after regaining authority");
                assert!(matches!(services.grant().read_invitation(&peer, other.id()).await, Err(Error::Absent)), "another admission manager cannot see this creator's code");
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let damage = |id: super::Id| { let owner = &owner; async move {
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                    owner.client().execute("UPDATE access.invitations SET hash = set_byte(hash, 0, get_byte(hash, 0) # 1) WHERE id = $1", &[&id]).await.expect("foreign digest");
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                } };
                damage(other.id()).await;
                assert!(matches!(services.grant().read_invitation(&peer, other.id()).await, Err(Error::Absent)), "another creator's damaged row is not decoded or revealed");
                services.ready().await.expect("an undecoded foreign row does not close admission");
                damage(created.id()).await;
                let error = services.grant().read_invitation(&member, created.id()).await.err().expect("a withheld code is still authenticated");
                assert_eq!(error.failure(), crate::access::grant::Failure::Unavailable);
                assert!(crate::runtime::work::Failure::unreliable(&error));
                assert!(services.ready().await.is_err(), "damaged facts close shared admission");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.invitations", &[]).await.expect("reads write nothing").get::<_, i64>(0), 2);
                owner.terminate().await.expect("owner ended"); observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded creator read");
        }));
    }
}
