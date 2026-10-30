//! Session logout and account-scoped observation of its permanent original set.

mod command;

use std::{convert::Infallible, num::NonZeroU64};

use bytes::BytesMut;
use chrono::{DateTime, Utc};
#[cfg(test)]
pub(crate) use command::COMPLETIONS;
use tokio_postgres::{
    Client,
    types::{FromSql, IsNull, ToSql, Type, to_sql_checked},
};

use crate::{
    identity::{
        current::{self, Actor},
        operation, session,
    },
    kernel::{self, App, operation::Family},
    runtime::registry,
};

pub type Error = kernel::Error<Infallible>;

pub(crate) async fn verify(client: &Client, app: &App, now: DateTime<Utc>) -> Result<(), Error> {
    command::verify(client, app, now).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::AsRefStr, strum::EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum Scope {
    Current,
    All,
}

impl<'a> FromSql<'a> for Scope {
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

impl ToSql for Scope {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().to_sql(kind, out)
    }

    fn accepts(kind: &Type) -> bool {
        <&str as ToSql>::accepts(kind)
    }

    to_sql_checked!();
}

/// A confirmed, nonempty original logout set.
pub struct Completion {
    count: NonZeroU64,
    revoked_at: DateTime<Utc>,
}

impl Completion {
    fn new(count: usize, revoked_at: DateTime<Utc>) -> Result<Self, Error> {
        let count = u64::try_from(count)
            .ok()
            .and_then(NonZeroU64::new)
            .ok_or(Error::corrupt(
                "a logout contains at least its initiating session",
            ))?;
        Ok(Self { count, revoked_at })
    }

    pub fn count(&self) -> NonZeroU64 {
        self.count
    }

    pub fn revoked_at(&self) -> DateTime<Utc> {
        self.revoked_at
    }
}

pub struct Revocation {
    id: session::Id,
    revoked_at: DateTime<Utc>,
}

impl Revocation {
    pub fn id(&self) -> session::Id {
        self.id
    }
    pub fn revoked_at(&self) -> DateTime<Utc> {
        self.revoked_at
    }
}

/// The binding and fixed set read from one verified permanent logout.
pub struct Original {
    scope: Scope,
    initiator_id: session::Id,
    revocations: Vec<Revocation>,
}

impl Original {
    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn initiator_id(&self) -> session::Id {
        self.initiator_id
    }

    pub fn revocations(&self) -> &[Revocation] {
        &self.revocations
    }
}

#[derive(Clone)]
pub struct Service {
    app: App,
    current: current::Service,
}

impl Service {
    pub(crate) fn prepare(app: App, current: current::Service) -> Self {
        Self { app, current }
    }

    pub async fn revoke(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        scope: Scope,
    ) -> Result<Completion, Error> {
        self.app
            .command(
                operation_id,
                command::Write {
                    service: self.clone(),
                    actor,
                    operation_id,
                    scope,
                },
            )
            .await
    }

    pub async fn read(
        &self,
        actor: &Actor,
        operation_id: operation::Id,
    ) -> Result<Option<Original>, Error> {
        self.app
            .registry()
            .wait_for(registry::Family::Kernel(Family::Logout), operation_id)
            .await?;
        self.app
            .query(&command::Original {
                service: self,
                actor,
                operation_id,
            })
            .await
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
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
            operation,
        },
        runtime::Context,
        startup::{Services, Settings},
    };

    async fn start(database: &Database, supplier: &Supplier) -> (Context, Services) {
        start_on(database.connections(&database.user), supplier).await
    }

    async fn start_on(
        connections: platform::database::Connections,
        supplier: &Supplier,
    ) -> (Context, Services) {
        let context = Context::new(connections).await.expect("context");
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

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    async fn issue(
        database: &Database,
        services: &Services,
        supplier: &mut Supplier,
    ) -> (Receipt, Actor) {
        issue_for(database, services, supplier, "13900000000").await
    }

    async fn issue_for(
        database: &Database,
        services: &Services,
        supplier: &mut Supplier,
        mobile: &str,
    ) -> (Receipt, Actor) {
        let id = message(database, services.authentication(), supplier, mobile).await;
        let receipt = Box::pin(services.authentication().create_session(input(
            Uuid::new_v4(),
            id,
            mobile,
            CODE,
        )))
        .await
        .expect("session");
        let Receipt::Session(issued) = &receipt else {
            panic!("existing account session")
        };
        let actor = services
            .current()
            .authenticate(Proof {
                id: issued.id(),
                token: issued.token().text().parse().expect("token"),
            })
            .await
            .expect("actor");
        (receipt, actor)
    }

    async fn register(database: &Database, services: &Services, supplier: &mut Supplier) -> Actor {
        let id = message(database, services.authentication(), supplier, "13800138000").await;
        let Receipt::Registration(credential) = services
            .authentication()
            .create_session(input(Uuid::new_v4(), id, "13800138000", CODE))
            .await
            .expect("registration credential")
        else {
            panic!("unoccupied mobile")
        };
        let issued = Box::pin(
            services
                .registration()
                .register(crate::registration::Input::new(
                    operation(),
                    credential.id(),
                    credential.token().text().parse().expect("token"),
                    "Other Operator".parse().expect("name"),
                    "00000020000229007X".parse().expect("identity number"),
                )),
        )
        .await
        .expect("actual R2 account");
        services
            .current()
            .authenticate(Proof {
                id: issued.id(),
                token: issued.token().text().parse().expect("session token"),
            })
            .await
            .expect("other account actor")
    }

    #[test]
    fn original_read_preserves_verified_results_when_commit_response_is_lost() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier).await;
                let (_, actor) = issue(&database, &services, &mut supplier).await;
                let (_, reader) = issue(&database, &services, &mut supplier).await;
                let operation_id = operation();
                let completion = services.revocation().revoke(actor.clone(), operation_id, super::Scope::Current).await.expect("original logout");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let query = "SELECT row_to_json(logout)::text AS original, NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=ANY(logout.sessions)) AS removed FROM identity.logouts logout WHERE id=$1";
                let before = observer.client().query_one(query, &[&operation_id]).await.expect("original facts before read");
                assert!(before.get::<_, bool>("removed"));
                let original: String = before.get("original");
                proxy.arm(Fault::LostReadCommit);
                let service = services.revocation().clone();
                let caller = tokio::spawn(async move { service.read(&reader, operation_id).await });
                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("read-only COMMIT reached");
                let result = timeout(Duration::from_secs(3), caller).await.expect("read finishes").expect("read task");
                proxy.release();
                let after = observer.client().query_one(query, &[&operation_id]).await.expect("independent facts before any subsequent read");
                assert_eq!(after.get::<_, String>("original"), original);
                assert!(after.get::<_, bool>("removed"));
                let original = result.expect("complete original result survives its read transaction cleanup").expect("original exists");
                assert_eq!(original.scope(), super::Scope::Current);
                assert_eq!(original.initiator_id(), actor.session_id());
                let results = original.revocations();
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].id(), actor.session_id());
                assert_eq!(results[0].revoked_at(), completion.revoked_at());
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                observer.terminate().await.expect("observer ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no external verification through read and drain");
                supplier.stop().await; proxy.stop().await;
            }).await.expect("read cleanup budget");
        });
    }

    #[test]
    fn all_logout_preserves_the_complete_r1_r2_set_through_cleanup_and_restart() {
        use chrono::TimeDelta;

        use crate::kernel::Command as _;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(35), async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(&database, &supplier).await;
                let registered = register(&database, &services, &mut supplier).await;
                let (_, previous) = issue_for(&database, &services, &mut supplier, "13800138000").await;
                let previous_operation = operation();
                services.revocation().revoke(previous.clone(), previous_operation, super::Scope::Current).await.expect("previous current logout");
                crate::registration::tests::age_facts(&database, &services, TimeDelta::hours(49)).await;
                let (_, actor) = issue_for(&database, &services, &mut supplier, "13800138000").await;
                let (_, member) = issue_for(&database, &services, &mut supplier, "13800138000").await;
                let (_, foreign) = issue(&database, &services, &mut supplier).await;
                let operation_id = operation();
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let mut authorities = Vec::new();
                for original in [&actor, &member, &foreign] {
                    let subject = services.current().fetch_state(
                        observer.client(), original, crate::history::Read::Shared,
                    ).await.expect("real subject before logout");
                    authorities.push(crate::kernel::Authorization {
                        person_id: subject.person_id(),
                        revision_id: subject.revision_id(),
                        now: subject.observed_at(),
                    });
                }
                let completion = services.revocation().revoke(actor.clone(), operation_id, super::Scope::All).await.expect("complete logout");
                assert_eq!(completion.count().get(), 3);
                let rows = observer.client().query("SELECT revocation.id,logout.initiator_id,logout.scope,revocation.revoked_at,revocation.transaction_id::text,session.expires_at<revocation.revoked_at AS expired,NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=revocation.id) AS removed,revocation.mode='logout' AND revocation.cause_id=logout.id AND revocation.operation_id=logout.id AND revocation.account_id=logout.account_id AND revocation.revoked_at=logout.revoked_at AND session.revoked_at=revocation.revoked_at AND session.sequence=2 AND session.version=2 AS paired FROM identity.logouts logout CROSS JOIN LATERAL unnest(logout.sessions) member(id) JOIN identity.revocations revocation ON revocation.id=member.id JOIN identity.sessions session ON session.id=member.id WHERE logout.id=$1 ORDER BY revocation.id", &[&operation_id]).await.expect("independent committed group before replay");
                assert_eq!(rows.len(), 3);
                let transaction: String = rows[0].get("transaction_id");
                let mut expected = vec![registered.session_id(), actor.session_id(), member.session_id()];
                expected.sort_by_key(ToString::to_string);
                assert_eq!(rows.iter().map(|row| row.get::<_, crate::identity::session::Id>("id")).collect::<Vec<_>>(), expected);
                for row in &rows {
                    assert_eq!(row.get::<_, crate::identity::session::Id>("initiator_id"), actor.session_id());
                    assert_eq!(row.get::<_, &str>("scope"), "all");
                    assert_eq!(row.get::<_, chrono::DateTime<chrono::Utc>>("revoked_at"), completion.revoked_at());
                    assert_eq!(row.get::<_, String>("transaction_id"), transaction);
                    assert!(row.get::<_, bool>("removed"));
                    assert!(row.get::<_, bool>("paired"));
                    assert_eq!(row.get::<_, bool>("expired"), row.get::<_, crate::identity::session::Id>("id") == registered.session_id());
                }
                assert_eq!(observer.client().query_one("SELECT operation_id FROM identity.revocations WHERE id=$1", &[&previous.session_id()]).await.expect("previous set unchanged").get::<_, operation::Id>(0), previous_operation);
                let write = super::command::Write {
                    service: services.revocation().clone(), actor: actor.clone(),
                    operation_id, scope: super::Scope::All,
                };
                let request = write.request(&authorities[0]).expect("original request binding");
                let record = crate::kernel::completion::find::<std::convert::Infallible>(
                    observer.client(), services.revocation().app.keys(),
                    <super::command::Write as crate::kernel::Command>::COMPLETIONS,
                    operation_id, &request,
                ).await.expect("original request binding lookup").expect("matching permanent completion");
                assert_eq!(record.created_at(), completion.revoked_at());
                for (original, scope, authority) in [
                    (&actor, super::Scope::Current, authorities[0]),
                    (&member, super::Scope::All, authorities[1]),
                    (&foreign, super::Scope::All, authorities[2]),
                ] {
                    let write = super::command::Write {
                        service: services.revocation().clone(), actor: original.clone(),
                        operation_id, scope,
                    };
                    let request = write.request(&authority).expect("changed original request binding");
                    assert!(matches!(crate::kernel::completion::find::<std::convert::Infallible>(
                        observer.client(), services.revocation().app.keys(),
                        <super::command::Write as crate::kernel::Command>::COMPLETIONS,
                        operation_id, &request,
                    ).await, Err(super::Error::Mismatch)));
                }
                assert!(services.revocation().read(&foreign, operation_id).await.expect("foreign group hidden").is_none());
                for old in [&actor, &member, &registered] {
                    assert!(matches!(services.current().read(old).await, Err(crate::identity::current::Error::Invalid)));
                }
                services.close(); services.drain().await.expect("drained with expired member retained"); context.stop().await.expect("stopped");
                let (context, services) = start(&database, &supplier).await;
                // Expiry, not this new revocation time, already ended the R2 member's retention.
                let mut cleanup = services.registration().retention();
                cleanup.run().await.expect("expired R2 member cleanup");
                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.sessions WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.issuances WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.logouts WHERE id=$2 AND $1=ANY(sessions))", &[&registered.session_id(),&operation_id]).await.expect("independent immediate cleanup").get::<_, bool>(0));
                let (_, reader) = issue_for(&database, &services, &mut supplier, "13800138000").await;
                assert!(services.current().read(&reader).await.is_ok(), "later transaction creates a valid session");
                for scope in [super::Scope::Current, super::Scope::All] {
                    assert!(matches!(services.revocation().revoke(reader.clone(), operation_id, scope).await, Err(super::Error::Mismatch)));
                }
                let read = services.revocation().read(&reader, operation_id).await.expect("complete permanent set").expect("original exists");
                assert_eq!(read.scope(), super::Scope::All);
                assert_eq!(read.initiator_id(), actor.session_id());
                assert_eq!(read.revocations().iter().map(super::Revocation::id).collect::<Vec<_>>(), expected);
                crate::registration::tests::age_facts(&database, &services, TimeDelta::hours(25)).await;
                let mut cleanup = services.authentication().retention();
                cleanup.run().await.expect("root and remaining R1 members expire");
                let row = observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.sessions WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.issuances WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$1) AS root_absent,(SELECT cardinality(sessions)::bigint FROM identity.logouts WHERE id=$2) AS count", &[&actor.session_id(),&operation_id]).await.expect("independent root cleanup before GET");
                assert!(row.get::<_, bool>("root_absent")); assert_eq!(row.get::<_, i64>("count"),3);
                let (receipt, reader) = issue_for(&database, &services, &mut supplier, "13800138000").await;
                let read = services.revocation().read(&reader, operation_id).await.expect("rootless session history retains the whole group").expect("original exists");
                assert_eq!(read.scope(), super::Scope::All);
                assert_eq!(read.initiator_id(), actor.session_id());
                assert_eq!(read.revocations().iter().map(super::Revocation::id).collect::<Vec<_>>(), expected);
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(&database, &supplier).await;
                let Receipt::Session(receipt) = receipt else { panic!("R1 session") };
                let reader = services.current().authenticate(Proof { id: receipt.id(), token: receipt.token().text().parse().expect("token") }).await.expect("new process actor");
                let read = services.revocation().read(&reader, operation_id).await.expect("restarted original").expect("original exists");
                assert_eq!(read.scope(), super::Scope::All);
                assert_eq!(read.initiator_id(), actor.session_id());
                assert_eq!(read.revocations().iter().map(super::Revocation::id).collect::<Vec<_>>(), expected);
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                observer.terminate().await.expect("observer ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no supplier calls through logout, recovery and drain");
                supplier.stop().await;
            }).await.expect("complete set budget");
        });
    }

    #[test]
    fn both_scopes_share_the_original_operation_and_wait_without_expanding_it() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                for scope in [super::Scope::Current, super::Scope::All] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier).await;
                    let (_, actor) = issue(&database, &services, &mut supplier).await;
                    let (_, member) = issue(&database, &services, &mut supplier).await;
                    let operation_id = operation();
                    proxy.arm(Fault::DelayedCommit);
                    let service = services.revocation().clone();
                    let original = actor.clone();
                    let first = tokio::spawn(async move { service.revoke(original, operation_id, scope).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original commit is unresolved");
                    let second_scope = if scope==super::Scope::All {super::Scope::Current} else {super::Scope::All};
                    let mut second = Box::pin(services.revocation().revoke(member.clone(), operation_id, second_scope));
                    let mut read = Box::pin(services.revocation().read(&member, operation_id));
                    assert!(timeout(Duration::from_millis(30), &mut second).await.is_err());
                    assert!(timeout(Duration::from_millis(30), &mut read).await.is_err(), "GET does not return an empty unresolved set");
                    proxy.release();
                    let completion = first.await.expect("first worker").expect("original set");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let row = observer.client().query_one("SELECT cardinality(sessions)::bigint AS count,scope=$2 AND initiator_id=$3 AS paired FROM identity.logouts WHERE id=$1", &[&operation_id,&scope,&actor.session_id()]).await.expect("original facts before repeated DELETE or GET results");
                    assert_eq!(row.get::<_, i64>("count"),if scope==super::Scope::All {2}else{1}); assert!(row.get::<_, bool>("paired"));
                    if scope==super::Scope::All {
                        assert!(matches!(second.await,Err(super::Error::Unauthenticated)));
                        assert!(matches!(read.await,Err(super::Error::Unauthenticated)));
                    } else {
                        assert!(matches!(second.await,Err(super::Error::Mismatch)));
                        let read = read.await.expect("original current set").expect("original exists");
                        assert_eq!(read.scope(), super::Scope::Current);
                        assert_eq!(read.initiator_id(), actor.session_id());
                        assert_eq!(read.revocations().len(),1);assert_eq!(read.revocations()[0].id(),actor.session_id());
                    }
                    assert_eq!(completion.count().get(),if scope==super::Scope::All {2}else{1});
                    observer.terminate().await.expect("observer ended");
                    services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;proxy.stop().await;
                }
            }).await.expect("shared operation budget");
        });
    }

    #[test]
    fn complete_group_reads_and_startup_reject_damaged_permanent_history() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(45), async {
                for damage in ["person", "initiator", "sessions", "time", "seal"] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(&database, &supplier).await;
                    let (_, actor) = issue(&database, &services, &mut supplier).await;
                    let (_, member) = issue(&database, &services, &mut supplier).await;
                    let foreign = register(&database, &services, &mut supplier).await;
                    let operation_id = operation();
                    let completion = services.revocation().revoke(actor.clone(), operation_id, super::Scope::All).await.expect("original complete group");
                    let (_, reader) = issue(&database, &services, &mut supplier).await;
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    let original: String = owner.client().query_one("SELECT row_to_json(logout)::text FROM identity.logouts logout WHERE id=$1", &[&operation_id]).await.expect("permanent original before damage").get(0);
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("history damage fixture");
                    match damage {
                        "person" => { owner.client().execute("UPDATE identity.logouts SET person_id=(SELECT person_id FROM identity.accounts WHERE id=$2) WHERE id=$1", &[&operation_id,&foreign.account_id()]).await.expect("wrong permanent person"); }
                        "initiator" => { owner.client().execute("UPDATE identity.logouts SET initiator_id=$2 WHERE id=$1", &[&operation_id,&member.session_id()]).await.expect("changed permanent initiator"); }
                        "sessions" => { owner.client().execute("UPDATE identity.logouts SET sessions=array_remove(sessions,$2) WHERE id=$1", &[&operation_id,&member.session_id()]).await.expect("missing permanent member"); }
                        "time" => { owner.client().execute("UPDATE identity.logouts SET revoked_at=revoked_at-interval '1 microsecond' WHERE id=$1", &[&operation_id]).await.expect("changed permanent time"); }
                        "seal" => { owner.client().execute("UPDATE identity.logouts SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE id=$1", &[&operation_id]).await.expect("damaged permanent seal"); }
                        _ => unreachable!("selected damage"),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let row = owner.client().query_one("SELECT row_to_json(logout)::text AS original,logout.person_id=account.person_id AS original_person,logout.initiator_id=$2 AS original_initiator,cardinality(logout.sessions) AS count,logout.revoked_at=$3 AS original_time,(SELECT count(*) FROM identity.revocations WHERE mode='logout' AND cause_id=$1 AND operation_id=$1) AS temporary_count FROM identity.logouts logout JOIN identity.accounts account ON account.id=logout.account_id WHERE logout.id=$1", &[&operation_id,&actor.session_id(),&completion.revoked_at()]).await.expect("independent damage before GET");
                    assert_ne!(row.get::<_, String>("original"), original);
                    assert_eq!(row.get::<_, bool>("original_person"), damage!="person");
                    assert_eq!(row.get::<_, bool>("original_initiator"), damage!="initiator");
                    assert_eq!(row.get::<_, i32>("count"), if damage=="sessions" {1} else {2});
                    assert_eq!(row.get::<_, bool>("original_time"), damage!="time");
                    assert_eq!(row.get::<_, i64>("temporary_count"), 2);
                    assert!(matches!(services.revocation().read(&reader, operation_id).await, Err(super::Error::Corrupt(_))), "never return a partial group: {damage}");
                    assert!(services.ready().await.is_err());
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    let context = Context::new(database.connections(&database.user)).await.expect("restart context");
                    let result = Services::start(Barrier::begin(context.clone()).await.expect("barrier"),authentication::inventory(keys()),Settings::new(4,crate::identity::message::Limits::default(),authentication::session_settings(),crate::registration::Limits::default(),authentication::avatar_settings()).expect("settings"),supplier.client(),authentication::unused_client().await).await;
                    assert!(matches!(result,Err(crate::startup::Error::Identity(crate::identity::verification::Error::History(_)))), "damaged group blocks startup: {damage}");
                    context.stop().await.expect("failed startup stopped"); owner.terminate().await.expect("owner ended"); supplier.stop().await;
                }
            }).await.expect("damaged groups budget");
        });
    }

    #[test]
    fn r1_original_delivery_rechecks_logout_after_its_account_lookup() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(45), async {
                for (recovering, failed_retry) in [(false, false), (true, false), (true, true)] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier).await;
                    let (_, actor) = issue(&database, &services, &mut supplier).await;
                    let message_id = message(&database, services.authentication(), &mut supplier, "13900000000").await;
                    let authentication_id = Uuid::new_v4();
                    if !recovering {
                        services.authentication().create_session(input(authentication_id, message_id, "13900000000", CODE)).await.expect("original session");
                    }
                    proxy.arm(if recovering { Fault::DelayedCommit } else { Fault::PausedAuthenticationSession });
                    let authentication = services.authentication().clone();
                    let task = tokio::spawn(async move {
                        authentication.create_session(input(authentication_id, message_id, "13900000000", CODE)).await
                    });
                    if recovering {
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original commit waits before losing its response");
                        proxy.arm(Fault::PausedAuthenticationSession);
                        proxy.release();
                    }
                    let backend = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original account lookup response is held");
                    let operation_id = operation();
                    let completion = services.revocation().revoke(actor, operation_id, super::Scope::All).await.expect("logout commits before the lookup response arrives");
                    assert_eq!(completion.count().get(), 2);
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    let row = observer.client().query_one("SELECT count(*)::bigint AS sessions,bool_and(session.revoked_at IS NOT NULL) AS revoked,(SELECT count(*) FROM identity.authentications WHERE operation_id=$1) AS authentications FROM identity.sessions session JOIN identity.authentications authentication ON authentication.id=session.authentication_id WHERE authentication.operation_id=$1", &[&authentication_id]).await.expect("original session facts before release");
                    assert_eq!(row.get::<_, i64>("sessions"), 1);
                    assert!(row.get::<_, bool>("revoked"));
                    assert_eq!(row.get::<_, i64>("authentications"), 1);
                    if failed_retry {
                        proxy.arm(Fault::RejectedRollback);
                    }
                    proxy.release();
                    let error = task.await.expect("R1 worker").err().expect("revoked original cannot deliver credentials");
                    if failed_retry {
                        assert_eq!(error.failure(), authentication::Failure::Unknown);
                        let authentication::Error::Unknown { recovery, .. } = error else { panic!("unknown original outcome") };
                        let recovery = match *recovery {
                            authentication::Error::Combined { cause, .. } => *cause,
                            error => error,
                        };
                        assert!(matches!(recovery, authentication::Error::History(crate::history::Error::Retry(_))), "{recovery:?}");
                    } else {
                        assert_eq!(error.failure(), authentication::Failure::Verification, "recovering={recovering}: {error:?}");
                    }
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    timeout(Duration::from_secs(3), async { loop {
                        let ended: bool = observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)", &[&backend]).await.expect("original read backend ended").get(0);
                        if ended { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("read connection cannot outlive its stopped context");
                    let count: i64 = observer.client().query_one("SELECT count(*) FROM identity.sessions session JOIN identity.authentications authentication ON authentication.id=session.authentication_id WHERE authentication.operation_id=$1", &[&authentication_id]).await.expect("no replacement session after all recovery").get(0);
                    assert_eq!(count, 1);
                    observer.terminate().await.expect("observer ended");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    supplier.stop().await;
                    proxy.stop().await;
                }
            }).await.expect("original lookup interleavings budget");
        });
    }

    #[test]
    fn all_logout_and_new_authentication_follow_the_account_lock_in_both_orders() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(40), async {
                for logout_first in [false, true] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier).await;
                    let (_, actor) = issue(&database, &services, &mut supplier).await;
                    let message_id = message(&database, services.authentication(), &mut supplier, "13900000000").await;
                    let authentication_id = Uuid::new_v4();
                    let operation_id = operation();
                    let authentication = services.authentication().clone();
                    let revocation = services.revocation().clone();
                    let original = actor.clone();
                    let create = Box::pin(async move { authentication.create_session(input(authentication_id, message_id, "13900000000", CODE)).await });
                    let revoke = Box::pin(async move { revocation.revoke(original, operation_id, super::Scope::All).await });
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    proxy.arm(Fault::DelayedCommit);
                    let (creation, logout) = if logout_first {
                        let logout = tokio::spawn(revoke);
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("all logout holds its final commit");
                        (tokio::spawn(create), logout)
                    } else {
                        let creation = tokio::spawn(create);
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("R1 holds its final commit");
                        (creation, tokio::spawn(revoke))
                    };
                    let query = if logout_first { "SELECT identity.lock_account%" } else { "SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE" };
                    timeout(Duration::from_secs(3), async { loop {
                        let waiting: bool = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE $1)", &[&query]).await.expect("actual opposing account waiter").get(0);
                        if waiting { break; } tokio::task::yield_now().await;
                    }}).await.expect("competing transaction coordinates before effects");
                    proxy.release();
                    let completed = logout.await.expect("logout worker").expect("logout completion");
                    let created = creation.await.expect("authentication worker");
                    let row = observer.client().query_one("SELECT session.id,EXISTS(SELECT 1 FROM identity.revocations WHERE id=session.id AND mode='logout' AND cause_id=$2 AND operation_id=$2 AND revoked_at=session.revoked_at) AS revoked,EXISTS(SELECT 1 FROM identity.handoffs WHERE id=session.id) AS delivered,(SELECT cardinality(sessions)::bigint FROM identity.logouts WHERE id=$2) AS count FROM identity.sessions session JOIN identity.authentications authentication ON authentication.id=session.authentication_id WHERE authentication.operation_id=$1", &[&authentication_id, &operation_id]).await.expect("independent original facts before R1 replay");
                    assert_eq!(row.get::<_, bool>("revoked"), !logout_first);
                    assert_eq!(row.get::<_, bool>("delivered"), logout_first);
                    assert_eq!(row.get::<_, i64>("count"), if logout_first { 1 } else { 2 });
                    assert_eq!(completed.count().get(), if logout_first { 1 } else { 2 });
                    if logout_first {
                        let Receipt::Session(issued) = created.expect("later login remains valid") else { panic!("R1 session") };
                        services.current().authenticate(Proof { id: issued.id(), token: issued.token().text().parse().expect("token") }).await.expect("later session valid");
                    } else {
                        // A commit acknowledgement can arrive before logout, or recovery can see
                        // the already revoked handoff. Neither authorizes a replacement session.
                        match created {
                            Ok(Receipt::Session(_)) => {},
                            Ok(Receipt::Registration(_)) => panic!("existing account cannot receive a registration credential"),
                            Err(error) => assert_eq!(error.failure(), crate::identity::authentication::Failure::Verification),
                        }
                        let error = services.authentication().create_session(input(authentication_id, message_id, "13900000000", CODE)).await.err().expect("revoked original handoff stays unavailable");
                        assert_eq!(error.failure(), crate::identity::authentication::Failure::Verification);
                    }
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await; proxy.stop().await;
                }
            }).await.expect("two account orders budget");
        });
    }

    #[test]
    fn runtime_sql_requires_a_complete_immutable_logout_group() {
        use tokio_postgres::error::SqlState;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(35), async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(&database, &supplier).await;
                let (_, actor) = issue(&database, &services, &mut supplier).await;
                let (_, member) = issue(&database, &services, &mut supplier).await;
                let foreign = register(&database, &services, &mut supplier).await;
                let runtime = database.connections(&database.user).recover().await.expect("runtime");
                for (defect, expected) in [
                    ("complete", None),
                    ("completion", Some((SqlState::FOREIGN_KEY_VIOLATION, "logouts_family_id_person_id_fkey"))),
                    ("session_record", Some((SqlState::FOREIGN_KEY_VIOLATION, "sessions_revocation_id_account_id_revoked_at_fkey"))),
                    ("session_state", Some((SqlState::FOREIGN_KEY_VIOLATION, "revocations_id_account_id_revoked_at_fkey"))),
                    ("cause", Some((SqlState::FOREIGN_KEY_VIOLATION, "revocations_logout_id_account_id_fkey"))),
                    ("account", Some((SqlState::FOREIGN_KEY_VIOLATION, "revocations_logout_id_account_id_fkey"))),
                    ("time", Some((SqlState::FOREIGN_KEY_VIOLATION, "revocations_id_account_id_revoked_at_fkey"))),
                    ("initiator", Some((SqlState::CHECK_VIOLATION, "logouts_check"))),
                    ("current_set", Some((SqlState::CHECK_VIOLATION, "logouts_check1"))),
                    ("account_sequence", Some((SqlState::CHECK_VIOLATION, "accounts_records_check"))),
                ] {
                    runtime.client().batch_execute("BEGIN").await.expect("rollback-only structure control");
                    let now = context.sample().await.expect("trusted time");
                    let operation_id = operation();
                    let sessions = if defect == "initiator" { vec![member.session_id()] } else { vec![actor.session_id(), member.session_id()] };
                    let sequence = if defect == "account_sequence" { 2_i64 } else { 1_i64 };
                    // These seals test SQL structure only and never leave this transaction.
                    let result: Result<(), tokio_postgres::Error> = async {
                        if defect != "completion" {
                            runtime.client().execute("INSERT INTO identity.completions(family,operation_id,action,target,input,subject,person_id,revision_id,created_at,seal,seal_key_id,seal_version) SELECT 'logout',$1,'logout',account.id::text,decode(repeat('00',37),'hex'),'session',account.person_id,person.revision_id,$2,decode(repeat('00',32),'hex'),account.seal_key_id,1 FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id WHERE account.id=$3", &[&operation_id, &now, &actor.account_id()]).await?;
                        }
                        runtime.client().execute("INSERT INTO identity.logouts(id,account_id,person_id,sequence,revoked_at,initiator_id,scope,sessions,seal,seal_key_id,seal_version) SELECT $1,id,person_id,sequence+$2,$3,$4,$5,$6,decode(repeat('00',32),'hex'),seal_key_id,1 FROM identity.accounts WHERE id=$7", &[&operation_id, &sequence, &now, &actor.session_id(), &if defect == "current_set" { "current" } else { "all" }, &sessions, &actor.account_id()]).await?;
                        runtime.client().execute("UPDATE identity.accounts SET sequence=sequence+$2,version=version+1,changed_at=$3,seal=decode(repeat('00',32),'hex') WHERE id=$1", &[&actor.account_id(), &sequence, &now]).await?;
                        for id in [member.session_id(), actor.session_id()] {
                            if defect != "session_record" {
                                let cause = if defect == "cause" { operation() } else { operation_id };
                                let account_id = if defect == "account" { foreign.account_id() } else { actor.account_id() };
                                let revoked_at = if defect == "time" { now - chrono::TimeDelta::microseconds(1) } else { now };
                                runtime.client().execute("INSERT INTO identity.revocations(id,account_id,sequence,mode,cause_id,operation_id,revoked_at,seal,seal_key_id,seal_version) SELECT id,$2,2,'logout',$3,$3,$4,decode(repeat('00',32),'hex'),seal_key_id,1 FROM identity.sessions WHERE id=$1", &[&id, &account_id, &cause, &revoked_at]).await?;
                            }
                            if defect != "session_state" {
                                runtime.client().execute("UPDATE identity.sessions SET sequence=2,version=2,revoked_at=$2,seal=decode(repeat('00',32),'hex') WHERE id=$1", &[&id, &now]).await?;
                            }
                        }
                        runtime.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await?;
                        runtime.client().execute("DELETE FROM identity.handoffs WHERE id=ANY($1)", &[&sessions]).await?;
                        runtime.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await
                    }.await;
                    runtime.client().batch_execute("ROLLBACK").await.expect("fake seals never commit");
                    match expected {
                        None => result.expect("complete structural control satisfies every deferred constraint"),
                        Some((expected, constraint)) => {
                            let error = result.expect_err("one missing or mismatched structural fact is rejected");
                            assert_eq!(error.code(), Some(&expected), "{defect}: {error}");
                            assert_eq!(error.as_db_error().expect("structural constraint").constraint(), Some(constraint), "{defect}");
                        }
                    }
                    let row = runtime.client().query_one("SELECT (SELECT count(*) FROM identity.revocations) AS count,(SELECT count(*) FROM identity.handoffs WHERE id IN($1,$2)) AS handoffs", &[&actor.session_id(), &member.session_id()]).await.expect("independent absence before another attempt");
                    assert_eq!(row.get::<_, i64>("count"), 0); assert_eq!(row.get::<_, i64>("handoffs"), 2);
                }
                let operation_id = operation();
                services.revocation().revoke(actor.clone(), operation_id, super::Scope::All).await.expect("real API commits the complete authenticated group");
                assert_eq!(runtime.client().query_one("SELECT count(*) FROM identity.revocations WHERE operation_id=$1", &[&operation_id]).await.expect("committed group before reads").get::<_, i64>(0), 2);
                let (_, later) = issue(&database, &services, &mut supplier).await;
                assert!(matches!(services.revocation().revoke(later.clone(), operation_id, super::Scope::All).await, Err(super::Error::Mismatch)));
                assert_eq!(runtime.client().query_one("SELECT count(*) FROM identity.revocations WHERE operation_id=$1", &[&operation_id]).await.expect("original set unchanged before replay").get::<_, i64>(0), 2);
                assert!(services.current().read(&later).await.is_ok());
                assert_eq!(services.revocation().read(&later, operation_id).await.expect("original group").expect("original exists").revocations().len(), 2);
                runtime.terminate().await.expect("runtime ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            }).await.expect("SQL group budget");
        });
    }

    #[test]
    fn current_logout_preserves_other_sessions_and_the_original_result_after_restart() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(&database, &supplier).await;
                let (_, actor) = issue(&database, &services, &mut supplier).await;
                let (receipt, other) = issue(&database, &services, &mut supplier).await;
                let operation_id = operation();
                let result = services.revocation().revoke(actor.clone(), operation_id, super::Scope::Current).await.expect("logout");
                assert_eq!(result.count().get(), 1);
                let observer = database.connections(&database.user).recover().await.expect("independent runtime observer");
                let row = observer.client().query_one("SELECT logout.account_id,logout.id AS operation_id,logout.revoked_at,session.account_id AS session_account,NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=ANY(logout.sessions)) AS removed,EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$2) AS other,logout.initiator_id=$1 AND logout.scope='current' AND logout.sessions=ARRAY[$1::uuid] AND revocation.mode='logout' AND revocation.cause_id=logout.id AND revocation.operation_id=logout.id AND revocation.account_id=logout.account_id AND revocation.revoked_at=logout.revoked_at AND session.revoked_at=logout.revoked_at AND session.sequence=2 AND session.version=2 AS paired FROM identity.logouts logout JOIN identity.sessions session ON session.id=logout.initiator_id JOIN identity.revocations revocation ON revocation.id=session.id WHERE logout.id=$3", &[&actor.session_id(),&other.session_id(),&operation_id]).await.expect("persisted original before replay");
                assert_eq!(row.get::<_, operation::Id>("operation_id"), operation_id);
                assert_eq!(row.get::<_, crate::identity::account::Id>("account_id"), row.get("session_account"));
                assert_eq!(row.get::<_, chrono::DateTime<chrono::Utc>>("revoked_at"), result.revoked_at());
                assert!(row.get::<_, bool>("removed") && row.get::<_, bool>("other"));
                assert!(row.get::<_, bool>("paired"));
                observer.terminate().await.expect("observer ended");
                assert!(matches!(services.revocation().revoke(actor.clone(), operation_id, super::Scope::Current).await, Err(super::Error::Unauthenticated)));
                assert!(matches!(services.revocation().revoke(other.clone(), operation_id, super::Scope::Current).await, Err(super::Error::Mismatch)));
                assert!(services.current().read(&other).await.is_ok());
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("context stopped");
                let (context, services) = start(&database, &supplier).await;
                let Receipt::Session(issued) = receipt else { panic!("session") };
                let other = services.current().authenticate(Proof { id: issued.id(), token: issued.token().text().parse().expect("token") }).await.expect("new process actor");
                let original = services.revocation().read(&other, operation_id).await.expect("original set").expect("original exists");
                assert_eq!(original.scope(), super::Scope::Current);
                assert_eq!(original.initiator_id(), actor.session_id());
                let results = original.revocations();
                assert_eq!(results.len(),1);
                assert_eq!(results[0].id(), actor.session_id());
                assert_eq!(results[0].revoked_at(), result.revoked_at());
                assert!(services.revocation().read(&other, operation()).await.expect("missing set").is_none());
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("context stopped");
                assert!(timeout(Duration::from_millis(50), supplier.request()).await.is_err(), "logout and reads have no supplier calls through drain");
                supplier.stop().await;
            }).await.expect("logout smoke budget");
        });
    }

    #[test]
    fn commit_faults_recover_only_the_original_fact_and_cancelled_waiters_leave_work_running() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(45), async {
                for fault in [Fault::LostCommit, Fault::DelayedCommit, Fault::RolledBack, Fault::UnknownCommit] {
                    let delayed = matches!(fault, Fault::DelayedCommit);
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    let unknown = matches!(fault, Fault::UnknownCommit);
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier).await;
                    let (_, actor) = issue(&database, &services, &mut supplier).await;
                    let (_, other) = issue(&database, &services, &mut supplier).await;
                    let operation_id = operation();
                    proxy.arm(fault);
                    let service = services.revocation().clone();
                    let original = actor.clone();
                    let task = tokio::spawn(async move { service.revoke(original, operation_id, super::Scope::Current).await });
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("business COMMIT intercepted");
                    if delayed {
                        task.abort();
                        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
                        let mut read = Box::pin(services.revocation().read(&other, operation_id));
                        assert!(timeout(Duration::from_millis(30), &mut read).await.is_err(), "GET observes the original running work");
                        proxy.release();
                        let observer = database.connections(&database.user).recover().await.expect("observer");
                        timeout(Duration::from_secs(2), async {
                            loop {
                                let committed: bool = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$1)", &[&actor.session_id()]).await.expect("independent commit observation").get(0);
                                if committed { break; }
                                tokio::task::yield_now().await;
                            }
                        }).await.expect("late backend commit");
                        observer.terminate().await.expect("observer ended");
                        assert_eq!(read.await.expect("original read").expect("original exists").revocations()[0].id(),actor.session_id());
                    } else {
                        let result = task.await.expect("worker");
                        if unknown {
                            assert!(matches!(result, Err(super::Error::Unknown(_))));
                            proxy.release();
                        } else if rolled_back {
                            assert!(matches!(result, Err(super::Error::Unavailable(_))));
                        } else {
                            assert_eq!(result.expect("recovered result").count().get(), 1);
                        }
                        let observer = database.connections(&database.user).recover().await.expect("observer");
                        // Cross the same original-operation barrier before observing a delayed backend.
                        crate::database::transaction::begin(&context, observer.client(), crate::kernel::operation::Family::Logout.lock(), operation_id).await.expect("observation barrier");
                        let row = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1) AS revoked, EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$1) AS handoff", &[&actor.session_id()]).await.expect("persisted outcome before replay");
                        assert_eq!(row.get::<_,bool>("revoked"),!rolled_back);
                        assert_eq!(row.get::<_,bool>("handoff"),rolled_back);
                        observer.client().batch_execute("COMMIT").await.expect("observation ended");
                        observer.terminate().await.expect("observer ended");
                        if rolled_back {
                            services.revocation().revoke(actor.clone(),operation_id,super::Scope::Current).await.expect("only confirmed rollback permits new attempt");
                        }
                        assert_eq!(services.revocation().read(&other,operation_id).await.expect("original set").expect("original exists").revocations()[0].id(),actor.session_id());
                        proxy.release();
                    }
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(25),supplier.request()).await.is_err());
                    supplier.stop().await; proxy.stop().await;
                }
            }).await.expect("commit faults budget");
        });
    }

    #[test]
    fn database_protects_the_original_session_pairing_time_history_and_permissions() {
        use tokio_postgres::error::SqlState;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database=Database::initialized(server).await;
                let mut supplier=Supplier::new().await;
                let (context,services)=start(&database,&supplier).await;
                let (_,actor)=issue(&database,&services,&mut supplier).await;
                let (_,other)=issue(&database,&services,&mut supplier).await;
                let runtime=database.connections(&database.user).recover().await.expect("runtime");
                for (sql,state) in [
                    ("UPDATE identity.accounts SET avatar=avatar WHERE id=(SELECT account_id FROM identity.sessions WHERE id=$1)",SqlState::CHECK_VIOLATION),
                    ("UPDATE identity.sessions SET expires_at=expires_at WHERE id=$1",SqlState::CHECK_VIOLATION),
                ] {
                    runtime.client().batch_execute("BEGIN").await.expect("negative transaction");
                    context.sample().await.expect("independent trusted sample");
                    let mut option=None;
                    for statement in sql.split(';') {
                        if let Err(error)=runtime.client().execute(statement,&[&actor.session_id()]).await { option=Some(error); break; }
                    }
                    let error=match option {Some(error)=>error,None=>runtime.client().batch_execute("COMMIT").await.expect_err("protected commit")};
                    assert_eq!(error.code(),Some(&state),"{error:?}");
                    runtime.client().batch_execute("ROLLBACK").await.expect("end rejected transaction");
                }
                let count:i64=runtime.client().query_one("SELECT count(*) FROM identity.revocations",&[]).await.expect("no partial write").get(0);
                assert_eq!(count,0);
                let operation_id=operation();
                services.revocation().revoke(actor.clone(),operation_id,super::Scope::Current).await.expect("valid logout");
                assert!(matches!(services.revocation().revoke(other.clone(),operation_id,super::Scope::Current).await,Err(super::Error::Mismatch)));
                for (sql, state) in [
                    ("DELETE FROM identity.revocations WHERE id=$1", SqlState::FOREIGN_KEY_VIOLATION),
                    ("UPDATE identity.revocations SET revoked_at=revoked_at WHERE id=$1", SqlState::INSUFFICIENT_PRIVILEGE),
                    ("DELETE FROM identity.issuances WHERE id=$1", SqlState::FOREIGN_KEY_VIOLATION),
                ] {
                    let error=runtime.client().execute(sql,&[&actor.session_id()]).await.expect_err("the retained session protects its paired records and runtime cannot rewrite a revocation");
                    assert_eq!(error.code(),Some(&state));
                }
                for sql in ["UPDATE identity.logouts SET revoked_at=revoked_at WHERE id=$1","DELETE FROM identity.logouts WHERE id=$1"] {
                    let error=runtime.client().execute(sql,&[&operation_id]).await.expect_err("permanent logout records have no runtime update or delete grant");
                    assert_eq!(error.code(),Some(&SqlState::INSUFFICIENT_PRIVILEGE));
                }
                let owner=database.connections("postgres").recover().await.expect("isolated permission drift");
                owner.client().batch_execute(&format!("GRANT UPDATE(revoked_at) ON identity.revocations TO {}",database.user)).await.expect("temporary extra grant");
                assert!(crate::database::permissions::verify(runtime.client(),&platform::database::User::new(database.user.clone()).expect("runtime user")).await.is_err());
                owner.client().batch_execute(&format!("REVOKE UPDATE(revoked_at) ON identity.revocations FROM {}",database.user)).await.expect("restore exact grants");
                crate::database::permissions::verify(runtime.client(),&platform::database::User::new(database.user.clone()).expect("runtime user")).await.expect("restored grants match the sole manifest");
                owner.terminate().await.expect("owner ended");
                runtime.client().batch_execute("SET enable_seqscan=off").await.expect("demonstrate available indexed plans on the small fixture");
                let plan=runtime.client().query("EXPLAIN SELECT id,account_id,revoked_at FROM identity.revocations WHERE mode='logout' AND operation_id=$1",&[&operation_id]).await.expect("logout result plan").into_iter().map(|row|row.get::<_,String>(0)).collect::<Vec<_>>().join("\n");
                assert!(plan.contains("revocations_mode_operation_id_id_idx"),"{plan}");
                runtime.client().batch_execute("RESET enable_seqscan").await.expect("restore planner");
                services.authentication().retention().run().await.expect("current retention keeps the recent revoked group");
                assert!(runtime.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.sessions WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.issuances WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1)",&[&actor.session_id()]).await.expect("24-hour retention is owned by the actual cleanup consumer").get::<_,bool>(0));
                assert_eq!(services.revocation().read(&other,operation_id).await.expect("unaltered result").expect("original exists").revocations().len(),1);
                runtime.terminate().await.expect("runtime ended");
                services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                supplier.stop().await;
            }).await.expect("SQL protection budget");
        });
    }

    #[test]
    fn account_locks_recheck_changed_qualification_and_serialize_competing_logouts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30),Box::pin(async {
                for invalidate in [false,true] {
                    let database=Database::initialized(Arc::clone(&server)).await;
                    let mut supplier=Supplier::new().await;
                    let (context,services)=start(&database,&supplier).await;
                    let (_,manager)=issue(&database,&services,&mut supplier).await;
                    let actor=if invalidate {Box::pin(register(&database,&services,&mut supplier)).await} else {manager.clone()};
                    let (_,member)=issue_for(&database,&services,&mut supplier,if invalidate {"13800138000"} else {"13900000000"}).await;
                    let option=if invalidate {Some(services.account().candidate(&manager,"13800138000".parse().expect("target mobile")).await.expect("real target confirmation"))}else{None};
                    let blocker=database.connections("postgres").recover().await.expect("blocker");
                    blocker.client().batch_execute("BEGIN").await.expect("begin");
                    let pid:i32=blocker.client().query_one("SELECT pg_backend_pid() FROM identity.accounts WHERE id=$1 FOR UPDATE",&[&actor.account_id()]).await.expect("account write lock").get(0);
                    let observer=database.connections(&database.user).recover().await.expect("observer");
                    let option=if let Some(candidate)=option {
                        let service=services.suspension().clone();
                        let account_id=candidate.identity().id();
                        let input=serde_json::from_value(serde_json::json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800138000"})).expect("typed suspension confirmation");
                        let task=tokio::spawn(async move {service.suspend(manager,operation(),account_id,input).await});
                        timeout(Duration::from_secs(3),async {loop {
                            if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND query = 'SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE')",&[&pid]).await.expect("suspension queued before logouts").get::<_,bool>(0){break;}
                            tokio::task::yield_now().await;
                        }}).await.expect("real suspension account wait");
                        Some(task)
                    } else {None};
                    let mut tasks=Vec::new();
                    for scope in [super::Scope::Current,super::Scope::All] {
                        let service=services.revocation().clone(); let actor=actor.clone();
                        tasks.push(tokio::spawn(async move {(scope,service.revoke(actor,operation(),scope).await)}));
                    }
                    timeout(Duration::from_secs(3),async {
                        loop {
                            let count:i64=observer.client().query_one("WITH RECURSIVE blocked(pid) AS (SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) UNION SELECT backend.pid FROM pg_stat_activity backend JOIN blocked ON blocked.pid=ANY(pg_blocking_pids(backend.pid)) WHERE backend.datname=current_database()) SELECT count(*) FROM pg_stat_activity WHERE pid IN (SELECT pid FROM blocked) AND wait_event_type='Lock' AND query = 'SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE'",&[&pid]).await.expect("both actual lock waiters, including tuple-lock queues").get(0);
                            if count==if invalidate {1}else{2} {break;} tokio::task::yield_now().await;
                        }
                    }).await.expect("available business connections reached the account lock; the remaining task waits for the bounded pool");
                    blocker.client().batch_execute("COMMIT").await.expect("release");
                    blocker.terminate().await.expect("blocker ended");
                    if let Some(task)=option {task.await.expect("suspension task").expect("real suspension precedes waiting logouts");}
                    let mut successes=0;
                    let mut count=0;
                    for task in tasks {
                        let (scope,result)=task.await.expect("task");
                        match result {
                            Ok(completion)=>{successes+=1;count=if scope==super::Scope::All {2}else{1};assert_eq!(completion.count().get(),u64::try_from(count).expect("positive count"));},
                            Err(super::Error::Unauthenticated)=>{},
                            Err(error)=>panic!("unexpected result {error:?}"),
                        }
                    }
                    assert_eq!(successes,usize::from(!invalidate));
                    let persisted:i64=observer.client().query_one("SELECT coalesce(sum(cardinality(sessions)),0)::bigint FROM identity.logouts",&[]).await.expect("persisted logout set").get(0);
                    assert_eq!(persisted,count);
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.revocations WHERE mode='suspension'",&[]).await.expect("complete independent suspension set").get::<_,i64>(0),if invalidate{2}else{0});
                    if !invalidate {
                        assert!(matches!(services.current().read(&actor).await,Err(crate::identity::current::Error::Invalid)));
                        assert_eq!(services.current().read(&member).await.is_ok(),count==1);
                    }
                    observer.terminate().await.expect("observer ended");
                    services.close();services.drain().await.expect("drain");context.stop().await.expect("stop");supplier.stop().await;
                }
            })).await.expect("concurrency budget");
        }));
    }

    #[test]
    fn original_operations_cannot_cross_accounts_and_queries_neither_disclose_nor_write() {
        use tokio_postgres::error::SqlState;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30),async {
                let database=Database::initialized(server).await;let mut supplier=Supplier::new().await;
                let (context,services)=start(&database,&supplier).await;
                let (_,actor)=issue(&database,&services,&mut supplier).await;
                let foreign=register(&database,&services,&mut supplier).await;
                let runtime=database.connections(&database.user).recover().await.expect("runtime");
                let operation_id=operation();
                let first=services.revocation().revoke(actor.clone(),operation_id,super::Scope::Current);
                let second=services.revocation().revoke(foreign.clone(),operation_id,super::Scope::All);
                let (first,second)=tokio::join!(first,second);
                let (winner,reader)=if first.is_ok() {assert!(matches!(second,Err(super::Error::Mismatch)));(&actor,&foreign)} else {assert!(matches!(first,Err(super::Error::Mismatch)));second.expect("one accepted");(&foreign,&actor)};
                let row=runtime.client().query_one("SELECT cardinality(sessions)::bigint AS count,account_id=$2 AND sessions=ARRAY[$3::uuid] AS paired FROM identity.logouts WHERE id=$1",&[&operation_id,&winner.account_id(),&winner.session_id()]).await.expect("original set before GET");
                assert_eq!(row.get::<_,i64>("count"),1);assert!(row.get::<_,bool>("paired"));
                runtime.client().batch_execute("BEGIN").await.expect("cross-account reference control");
                let now=context.sample().await.expect("trusted sample");
                let error=runtime.client().execute("INSERT INTO identity.revocations(id,account_id,sequence,mode,cause_id,operation_id,revoked_at,seal,seal_key_id,seal_version) SELECT id,account_id,2,'logout',$2,$2,$3,decode(repeat('00',32),'hex'),seal_key_id,1 FROM identity.sessions WHERE id=$1",&[&reader.session_id(),&operation_id,&now]).await.expect_err("a session cannot reference the other account's permanent logout");
                assert_eq!(error.code(),Some(&SqlState::FOREIGN_KEY_VIOLATION));
                runtime.client().batch_execute("ROLLBACK").await.expect("cross-account fake seal never commits");
                let before:String=runtime.client().query_one("SELECT json_agg(logout ORDER BY id)::text FROM identity.logouts logout",&[]).await.expect("original rows").get(0);
                assert!(services.revocation().read(reader,operation_id).await.expect("foreign empty set").is_none());
                assert!(services.revocation().read(reader,operation()).await.expect("nonexistent empty set").is_none());
                let after:String=runtime.client().query_one("SELECT json_agg(logout ORDER BY id)::text FROM identity.logouts logout",&[]).await.expect("unchanged rows").get(0);assert_eq!(before,after);
                runtime.terminate().await.expect("runtime ended");services.close();services.drain().await.expect("drain");context.stop().await.expect("stop");supplier.stop().await;
            }).await.expect("cross-account budget");
        });
    }

    #[test]
    fn original_revocation_survives_whole_session_cleanup_and_preserves_its_fixed_set() {
        use chrono::TimeDelta;

        use crate::identity::authentication::session::tests::age;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30),async {
                let database=Database::initialized(server).await;let mut supplier=Supplier::new().await;
                let (context,services)=start(&database,&supplier).await;
                let (receipt,actor)=issue(&database,&services,&mut supplier).await;
                let authentication::session::Receipt::Session(original)=receipt else {panic!("session");};
                let operation_id=operation();let completed=services.revocation().revoke(actor.clone(),operation_id,super::Scope::Current).await.expect("logout");
                age(&database, services.authentication(),TimeDelta::hours(25)).await;
                let mut retention=services.authentication().retention();
                retention.run().await.expect("legal session cleanup");
                let runtime=database.connections(&database.user).recover().await.expect("runtime");
                assert!(runtime.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.sessions WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.issuances WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.logouts WHERE id=$2 AND sessions=ARRAY[$1::uuid])",&[&actor.session_id(),&operation_id]).await.expect("independent cleanup observation").get::<_,bool>(0));
                let (_,other)=issue(&database,&services,&mut supplier).await;
                assert_ne!(other.session_id(),actor.session_id());
                assert!(matches!(services.current().authenticate(Proof{id:original.id(),token:original.token().text().parse().expect("original token")}).await,Err(crate::identity::current::Error::Invalid)));
                let original=services.revocation().read(&other,operation_id).await.expect("account-lifetime result").expect("original exists");
                assert_eq!(original.scope(), super::Scope::Current);
                assert_eq!(original.initiator_id(), actor.session_id());
                let result = original.revocations();
                assert_eq!(result.len(),1);assert_eq!(result[0].id(),actor.session_id());assert_eq!(result[0].revoked_at(),completed.revoked_at()-TimeDelta::hours(25));
                assert!(matches!(services.revocation().revoke(other.clone(),operation_id,super::Scope::Current).await,Err(super::Error::Mismatch)));
                let row=runtime.client().query_one("SELECT account_id=$3 AND initiator_id=$1 AND scope='current' AND sessions=ARRAY[$1::uuid] AND revoked_at=$4 AS original,NOT EXISTS(SELECT 1 FROM identity.sessions WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.issuances WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$1) AS cleaned FROM identity.logouts WHERE id=$2",&[&actor.session_id(),&operation_id,&actor.account_id(),&(completed.revoked_at()-TimeDelta::hours(25))]).await.expect("new authentication cannot extend the permanent old set or revive its credential");
                assert!(row.get::<_,bool>("original"));assert!(row.get::<_,bool>("cleaned"));
                runtime.terminate().await.expect("runtime ended");services.close();services.drain().await.expect("drain");context.stop().await.expect("stop");supplier.stop().await;
            }).await.expect("retention and recreation budget");
        });
    }

    #[test]
    fn startup_rejects_tampered_expiry_on_retained_revoked_sessions() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                for offset in [0_i64, 1] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(&database, &supplier).await;
                    let (_, actor) = issue(&database, &services, &mut supplier).await;
                    services.revocation().revoke(actor.clone(), operation(), super::Scope::Current).await.expect("valid current logout");
                    services.close();
                    services.drain().await.expect("original services drained");
                    context.stop().await.expect("original clock stopped");
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("fixture-only corruption");
                    owner.client().execute(
                        "UPDATE identity.sessions session SET expires_at=revocation.revoked_at-$2::bigint*interval '1 microsecond' FROM identity.revocations revocation WHERE session.id=$1 AND revocation.id=session.id",
                        &[&actor.session_id(), &offset],
                    ).await.expect("retained session expires at or before its revocation");
                    owner.client().batch_execute("COMMIT").await.expect("fixture corruption committed");
                    let damaged: bool = owner.client().query_one(
                        "SELECT session.created_at<session.expires_at AND session.expires_at<=revocation.revoked_at AND revocation.revoked_at<clock_timestamp() FROM identity.sessions session JOIN identity.revocations revocation ON revocation.id=session.id WHERE session.id=$1",
                        &[&actor.session_id()],
                    ).await.expect("independent invalid lifetime observation").get(0);
                    assert!(damaged, "the original session expiry was changed across its revocation");
                    let context = Context::new(database.connections(&database.user)).await.expect("restart context");
                    let result = Services::start(
                        Barrier::begin(context.clone()).await.expect("restart barrier"),
                        authentication::inventory(keys()),
                        Settings::new(4, crate::identity::message::Limits::default(), authentication::session_settings(), crate::registration::Limits::default(), authentication::avatar_settings()).expect("settings"),
                        supplier.client(),
                        authentication::unused_client().await,
                    ).await;
                    match result {
                        Err(crate::startup::Error::Identity(crate::identity::verification::Error::History(
                            crate::history::Error::Seal(crate::keys::seal::Error::Mismatch),
                        ))) => {},
                        Err(error) => panic!("the damaged Session retains its seal error: {error:?}"),
                        Ok(services) => {
                            services.close();
                            services.drain().await.expect("unexpectedly accepted services drained");
                            panic!("startup accepted a changed Session expiry");
                        },
                    }
                    context.stop().await.expect("restart clock stopped");
                    owner.terminate().await.expect("corruption owner ended");
                    supplier.stop().await;
                }
            }).await.expect("startup lifetime budget");
        });
    }

    #[test]
    fn decisive_get_rechecks_its_actor_and_startup_rejects_damaged_revocation_consequences() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30),Box::pin(async {
                for damage in ["revocation", "handoff", "logout"] {
                    let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;
                    let (context,services)=start(&database,&supplier).await;
                    let (_,actor)=issue(&database,&services,&mut supplier).await;
                    let (_,other)=issue(&database,&services,&mut supplier).await;
                    let foreign=register(&database,&services,&mut supplier).await;
                    let operation_id=operation();services.revocation().revoke(actor.clone(),operation_id,super::Scope::Current).await.expect("logout");
                    let owner=database.connections("postgres").recover().await.expect("isolated corruption");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("explicit storage damage");
                    if damage == "revocation" {
                        owner.client().execute("UPDATE identity.revocations SET account_id=$2 WHERE id=$1",&[&actor.session_id(),&foreign.account_id()]).await.expect("wrong revocation owner");
                    } else if damage == "handoff" {
                        owner.client().execute("INSERT INTO identity.handoffs(id,token,expires_at) SELECT $1,token,expires_at FROM identity.handoffs WHERE id=$2",&[&actor.session_id(),&other.session_id()]).await.expect("forbidden retained handoff");
                    } else {
                        owner.client().execute("UPDATE identity.logouts SET revoked_at=revoked_at+interval '1 microsecond' WHERE id=$1", &[&operation_id]).await.expect("damage the permanent original result");
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage committed only by fixture");
                    if damage == "logout" {
                        let error = services.revocation().read(&other,operation_id).await.err().expect("GET authenticates its permanent original record");
                        assert!(matches!(error, super::Error::Corrupt(ref fault)
                            if matches!(fault.kind(), crate::kernel::FaultKind::History(crate::history::Error::Seal(crate::keys::seal::Error::Mismatch)))), "{error:?}");
                        assert!(services.ready().await.is_err());
                    } else {
                        assert!(matches!(services.current().read(&actor).await,Err(crate::identity::current::Error::Invalid)));
                        let original = services.revocation().read(&other,operation_id).await.expect("the authenticated permanent set does not rescan temporary consequences").expect("original exists");
                        let result = original.revocations();
                        assert_eq!(result.len(), 1); assert_eq!(result[0].id(), actor.session_id());
                        services.ready().await.expect("unconsumed damage is discovered by the complete startup scan");
                    }
                    services.close();services.drain().await.expect("drain");context.stop().await.expect("stop");
                    let context=Context::new(database.connections(&database.user)).await.expect("restart context");
                    let result=Services::start(Barrier::begin(context.clone()).await.expect("barrier"),authentication::inventory(keys()),Settings::new(4,crate::identity::message::Limits::default(),authentication::session_settings(),crate::registration::Limits::default(),authentication::avatar_settings()).expect("settings"),supplier.client(),authentication::unused_client().await).await;
                    let error = result.err().expect("no service can escape the damaged startup graph");
                    if damage == "handoff" {
                        assert!(matches!(error, crate::startup::Error::Authentication(_)), "{damage}: {error:?}");
                    } else {
                        assert!(matches!(error, crate::startup::Error::Identity(crate::identity::verification::Error::History(
                            crate::history::Error::Seal(crate::keys::seal::Error::Mismatch),
                        ))), "{damage}: {error:?}");
                    }
                    context.stop().await.expect("failed startup ended");
                    owner.terminate().await.expect("owner ended");supplier.stop().await;
                }
                let database=Database::initialized(server).await;let mut supplier=Supplier::new().await;let (context,services)=start(&database,&supplier).await;
                let (_,actor)=issue(&database,&services,&mut supplier).await;
                let owner=database.connections("postgres").recover().await.expect("expiry fixture");
                let expires_at: chrono::DateTime<chrono::Utc> = owner.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1", &[&actor.session_id()]).await.expect("original expiry").get(0);
                crate::identity::authentication::session::tests::age(&database, services.authentication(), expires_at-database.now().await+chrono::TimeDelta::seconds(1)).await;
                assert!(matches!(services.revocation().read(&actor,operation()).await,Err(super::Error::Unauthenticated)));
                assert!(matches!(services.revocation().revoke(actor,operation(),super::Scope::Current).await,Err(super::Error::Unauthenticated)));
                owner.terminate().await.expect("owner ended");services.close();services.drain().await.expect("drain");context.stop().await.expect("stop");supplier.stop().await;
            })).await.expect("decisive and startup budget");
        }));
    }
}
