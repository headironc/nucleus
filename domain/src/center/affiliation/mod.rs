//! Sealed person-owned affiliations, qualified correction and permanent completion.

pub mod admin;
mod code;
mod command;
mod database;
mod envelope;
pub mod management;
mod profile;
mod query;

use std::{collections::HashSet, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::{
    Client,
    types::{FromSql, ToSql},
};
use utoipa::ToSchema;
use uuid::Uuid;

pub use crate::center::affiliation::{
    code::Code,
    management::{Candidate, Target},
};
pub(crate) use crate::center::affiliation::{
    database::TABLE as AFFILIATIONS, profile::TABLE as PROFILES,
};
#[cfg(any(test, feature = "test-support"))]
use crate::history;
use crate::{
    access::role,
    identity::{
        self, account,
        current::{self, Actor},
        operation, person,
    },
    kernel::{
        self, App,
        completion::{self, Template},
        operation::Family,
    },
    pagination::Pagination,
};

const COMPLETIONS: Template = Template::personal(Family::CenterAffiliation, |action, target| {
    matches!(action, "fill" | "correct" | "system_correct")
        && target
            .parse::<person::Id>()
            .is_ok_and(|id| id.to_string() == target)
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql)]
#[serde(transparent)]
#[postgres(transparent)]
struct Id(Uuid);

impl std::fmt::Display for Id {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Id {
    fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = affiliation::Input)]
pub struct Input {
    affiliation: Code,
}

enum Request {
    Fill(Code),
    Correct { input: management::Input },
}

impl Request {
    fn code(&self) -> Code {
        match self {
            Self::Fill(code) => *code,
            Self::Correct { input } => input.affiliation,
        }
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = affiliation::Affiliation)]
pub struct Affiliation {
    value: Code,
    created_at: DateTime<Utc>,
}

pub struct List {
    count: i64,
    affiliations: Vec<Affiliation>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn affiliations(&self) -> &[Affiliation] {
        &self.affiliations
    }
}

#[derive(Clone)]
pub struct Service {
    app: App,
    current: current::Service,
    roles: role::Reader,
    accounts: account::Reader,
    persons: person::Reader,
    revisions: person::revision::Reader,
}

pub(crate) async fn verify(client: &Client, app: &App, now: DateTime<Utc>) -> Result<(), Error> {
    profile::verify(client, app.keys(), app.anchors(), now).await?;
    let completions = completion::fetch_all(client, app.keys(), COMPLETIONS).await?;
    let mut operations = HashSet::with_capacity(completions.len());
    for record in &completions {
        if record.created_at() > now {
            return Err(Error::corrupt(
                "completion time does not exceed startup time",
            ));
        }
        if !operations.insert(record.operation_id()) {
            return Err(Error::corrupt(
                "each retained completion keeps its original operation",
            ));
        }
    }
    for operation in database::operations(client).await? {
        if !operations.remove(&operation) {
            return Err(Error::corrupt(
                "a retained affiliation keeps its original completion",
            ));
        }
    }
    if !operations.is_empty() {
        return Err(Error::corrupt(
            "a retained completion keeps its original affiliation",
        ));
    }
    Ok(())
}

/// Disposable fixtures write one complete first value through the production encoding.
#[cfg(any(test, feature = "test-support"))]
pub async fn seed(
    client: &Client,
    keys: &crate::keys::Keys,
    person_id: person::Id,
    operator_id: person::revision::Id,
    code: Code,
    operation_id: operation::Id,
) -> Result<(), Error> {
    let created_at: DateTime<Utc> = client
        .query_one("SELECT clock_timestamp()", &[])
        .await
        .map_err(Error::query)?
        .try_get(0)
        .map_err(Error::decode)?;
    let id = Id::allocate();
    let draft = history::Draft::<profile::Profile>::create(profile::Data {
        person_id,
        affiliation_id: id,
        created_at,
    });
    history::Change::new(
        draft,
        database::Data {
            id,
            code,
            operator_id,
            #[cfg(test)]
            operator_person: person_id,
            created_at,
            operation_id,
        },
    )
    .write_offline(
        client,
        history::Keys {
            data: &keys.encryption,
            seals: &keys.seal,
        },
    )
    .await?;
    #[derive(serde::Serialize)]
    struct Input {
        affiliation: Code,
    }
    let authority = kernel::Authorization {
        person_id,
        revision_id: operator_id,
        now: created_at,
    };
    let request = completion::Request::new(
        "fill",
        person_id.to_string(),
        &Input { affiliation: code },
        &authority,
    )?;
    completion::insert(
        client,
        history::Keys {
            data: &keys.encryption,
            seals: &keys.seal,
        },
        COMPLETIONS,
        operation_id,
        &request,
        created_at,
        COMPLETIONS.expires(created_at),
    )
    .await?;
    Ok(())
}

impl Service {
    pub(crate) fn prepare(
        app: App,
        current: current::Service,
        identity: Arc<identity::keys::Keys>,
    ) -> Self {
        let accounts = account::Reader::new(app.clone(), Arc::clone(&identity));
        Self {
            roles: role::Reader::new(app.clone()),
            app: app.clone(),
            current,
            accounts: accounts.clone(),
            persons: person::Reader::new(accounts, Arc::clone(&identity)),
            revisions: person::revision::Reader::new(app),
        }
    }

    pub async fn values(&self, actor: &Actor) -> Result<&'static [Code], Error> {
        self.app
            .query(&query::Values {
                service: self,
                actor,
            })
            .await
    }

    pub async fn read(&self, actor: &Actor) -> Result<Affiliation, Error> {
        self.app
            .query(&query::Current {
                service: self,
                actor,
                target: None,
            })
            .await
    }

    pub async fn history(&self, actor: &Actor, pagination: Pagination) -> Result<List, Error> {
        self.app
            .query(&query::History {
                service: self,
                actor,
                pagination,
            })
            .await
    }

    pub async fn create(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<(), Error> {
        self.submit(actor, operation_id, Request::Fill(input.affiliation))
            .await
    }

    async fn submit(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        request: Request,
    ) -> Result<(), Error> {
        self.app
            .command(
                operation_id,
                command::Write {
                    service: self.clone(),
                    actor,
                    request,
                    id: Id::allocate(),
                },
            )
            .await
    }
}

pub type Error = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Absent,
    Unset,
    Conflict,
    TargetChanged,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Absent | Self::Unset => kernel::Status::NotFound,
            Self::Conflict | Self::TargetChanged => kernel::Status::Conflict,
        }
    }
    fn code(&self) -> &'static str {
        match self {
            Self::Absent => "target_unavailable",
            Self::Unset => "affiliation_unset",
            Self::Conflict => "state_conflict",
            Self::TargetChanged => "target_changed",
        }
    }
}

impl From<Rejection> for Error {
    fn from(rejection: Rejection) -> Self {
        Self::Rejected(rejection)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use chrono::{DateTime, Utc};
    use platform::cryptography::envelope::Envelope;
    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use crate::{
        access::{
            appointment::tests::register,
            role::tests::{actor, start},
        },
        center::affiliation::{self, Code, Error, Id, Input, Rejection, envelope},
        database::sealed::Sealed,
        history,
        identity::{
            authentication::{self, Database, Supplier, server},
            operation, person,
        },
        kernel::{FaultKind, Stage, keys::Keys},
        keys::{Entry, Material},
        pagination::Pagination,
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }
    fn input(code: &str) -> Input {
        serde_json::from_value(json!({"affiliation":code})).expect("closed input")
    }

    #[test]
    fn fixed_values_require_a_current_session_without_management_or_workforce_authority() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    start(database.connections(&database.user), &supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let values = services
                    .affiliation()
                    .values(&member)
                    .await
                    .expect("ordinary valid session");
                assert_eq!(values, Code::all());
                services
                    .revocation()
                    .revoke(
                        member.clone(),
                        operation(),
                        crate::identity::revocation::Scope::Current,
                    )
                    .await
                    .expect("actual logout");
                assert!(
                    matches!(
                        services.affiliation().values(&member).await,
                        Err(Error::Unauthenticated)
                    ),
                    "an earlier Actor cannot bypass decisive session validation"
                );
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                assert!(
                    timeout(Duration::from_millis(30), supplier.request())
                        .await
                        .is_err()
                );
                supplier.stop().await;
            })
            .await
            .expect("bounded fixed values query");
        });
    }

    /// Rebuild this disposable person's retained history, then publish its verified fixture anchor.
    pub(super) async fn seed_history(
        client: &tokio_postgres::Client,
        service: &affiliation::Service,
        person_id: person::Id,
        mut records: Vec<affiliation::database::Data>,
    ) {
        use affiliation::{
            database,
            profile::{self, Profile},
        };

        use crate::{
            history::{Change, Draft, Read, Seed, State as _},
            kernel::{Authorization, completion},
        };
        let now = service.app.context().sample().await.expect("fixture time");
        if let Some(state) = history::find::<Profile>(
            client,
            service.app.keys(),
            service.app.anchors(),
            Read::Snapshot(service.app.anchors().view().expect("fixture view")),
            person_id,
            now,
        )
        .await
        .expect("existing fixture state")
        {
            for record in
                history::page(client, service.app.keys(), &state, state.sequence(), 0, now)
                    .await
                    .expect("existing fixture history")
            {
                use crate::history::Record as _;
                records.push(database::Data {
                    id: record.id(),
                    code: record.code(),
                    operator_id: record.operator_id(),
                    operator_person: service
                        .revisions
                        .fetch_identity(client, record.operator_id())
                        .await
                        .expect("fixed fixture operator")
                        .person_id(),
                    created_at: record.created_at(),
                    operation_id: record.operation_id(),
                });
            }
        }
        records.sort_by_key(|record| record.created_at);
        client
            .batch_execute("BEGIN")
            .await
            .expect("fixture transaction");
        client
            .execute(
                "DELETE FROM center.affiliations WHERE person_id=$1",
                &[&person_id],
            )
            .await
            .expect("replace fixture records");
        client
            .execute("DELETE FROM center.profiles WHERE id=$1", &[&person_id])
            .await
            .expect("replace fixture state");
        client
            .execute(
                "DELETE FROM center.completions WHERE target=$1",
                &[&person_id.to_string()],
            )
            .await
            .expect("replace fixture completions");
        let mut previous = None;
        for record in records {
            let data = profile::Data {
                person_id,
                affiliation_id: record.id,
                created_at: record.created_at,
            };
            let draft = match &previous {
                None => Draft::<Profile>::create(data),
                Some(state) => Draft::advance(state, data).expect("fixture advance"),
            };
            let authority = Authorization {
                person_id: record.operator_person,
                revision_id: record.operator_id,
                now: record.created_at,
            };
            #[derive(serde::Serialize)]
            struct Input {
                affiliation: Code,
            }
            let request = completion::Request::new::<affiliation::Rejection>(
                if record.operator_person == person_id {
                    "fill"
                } else {
                    "correct"
                },
                person_id.to_string(),
                &Input {
                    affiliation: record.code,
                },
                &authority,
            )
            .expect("fixture input");
            completion::insert::<affiliation::Rejection>(
                client,
                service.app.keys(),
                affiliation::COMPLETIONS,
                record.operation_id,
                &request,
                record.created_at,
                affiliation::COMPLETIONS.expires(record.created_at),
            )
            .await
            .expect("fixture completion");
            previous = Some(
                Seed::write(client, service.app.keys(), Change::new(draft, record))
                    .await
                    .expect("fixture state and record"),
            );
        }
        client
            .batch_execute("COMMIT")
            .await
            .expect("fixture commit");
        Seed::anchor(
            service.app.anchors(),
            &previous.expect("nonempty fixture history"),
        )
        .expect("committed fixture anchor");
    }

    async fn verify_history(
        client: &tokio_postgres::Client,
        keys: history::Keys<'_>,
        now: DateTime<Utc>,
    ) -> Result<(), history::Error> {
        let anchors = history::Anchors::new(tokio::sync::watch::channel(false).0);
        affiliation::profile::verify(client, keys, &anchors, now).await
    }

    #[test]
    fn history_preserves_person_ownership_and_original_effective_times() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let account = services.current().read(&member).await.expect("ordinary account");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let page = || Pagination::new(1, 20).expect("page");
                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND person_id=$1) AND NOT EXISTS(SELECT 1 FROM access.holders holder JOIN access.permissions permission ON permission.role_id=holder.role_id WHERE holder.person_id=$1)", &[&account.person_id()]).await.expect("no Workforce membership or management authority").get::<_,bool>(0));
                let empty = services.affiliation().history(&member, page()).await.expect("unfilled history");
                assert_eq!(empty.count(), 0); assert!(empty.affiliations().is_empty());
                let original = operation();
                services.affiliation().create(member.clone(), original, input("01")).await.expect("actual first fill");
                let row = observer.client().query_one("SELECT value,created_at FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("independent original facts");
                let value: Vec<u8> = row.get("value"); let created_at: DateTime<Utc> = row.get("created_at");
                let list = services.affiliation().history(&member, page()).await.expect("own history");
                assert_eq!(list.count(), 1); assert_eq!(list.affiliations().len(), 1);
                assert_eq!(serde_json::to_value(&list.affiliations()[0]).expect("minimal self projection"), json!({"value":"01","createdAt":created_at}));
                assert_eq!(services.affiliation().history(&administrator, page()).await.expect("separate person").count(), 0);
                Box::pin(services.correction().correct(administrator, operation(), serde_json::from_value(json!({"target":{"personId":account.person_id(),"revisionId":account.revision_id()},"name":"Corrected Person","idNumber":"00000020000229007X"})).expect("correction input"))).await.expect("actual real-name correction");
                let list = services.affiliation().history(&member, page()).await.expect("same permanent person");
                assert_eq!(list.affiliations()[0].value, Code::CommunistParty); assert_eq!(list.affiliations()[0].created_at, created_at);
                let account = services.current().read(&member).await.expect("corrected current identity");
                Box::pin(services.cancellation().cancel(member.clone(), operation(), serde_json::from_value(json!({"accountId":account.id(),"personId":account.person_id(),"revisionId":account.revision_id()})).expect("cancellation input"))).await.expect("actual cancellation");
                assert!(matches!(services.affiliation().history(&member, page()).await, Err(Error::Unauthenticated)));
                let replacement = register(&database, &services, &mut supplier).await;
                assert_ne!(services.current().read(&replacement).await.expect("new person").person_id(), account.person_id());
                let list = services.affiliation().history(&replacement, page()).await.expect("same phone does not inherit history");
                assert_eq!(list.count(), 0); assert!(list.affiliations().is_empty());
                let row = observer.client().query_one("SELECT count(*),bool_and(operation_id=$1 AND person_id=$2 AND value=$3 AND created_at=$4) FROM center.affiliations", &[&original,&account.person_id(),&value,&created_at]).await.expect("independent permanent facts");
                assert_eq!(row.get::<_,i64>(0), 1); assert!(row.get::<_,bool>(1));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded person-owned history");
        }));
    }

    #[test]
    fn history_pages_retained_values_by_effective_time_without_mutation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let account = services.current().read(&actor).await.expect("person");
                let original = operation();
                services.affiliation().create(actor.clone(), original, input("01")).await.expect("actual first fill");
                let owner = database.connections("postgres").recover().await.expect("retained fixture owner");
                let created_at: DateTime<Utc> = owner.client().query_one("SELECT created_at FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("original time").get(0);
                // Retained material isolates pagination from the separately tested write protocol.
                let records = (1..=100).map(|offset| {
                    let id = if offset == 1 { Id(Uuid::from_u128(u128::MAX)) } else { Id::allocate() };
                    affiliation::database::Data { id, code: Code::Unaffiliated, operator_id: account.revision_id(), operator_person: account.person_id(), created_at: created_at-chrono::Duration::microseconds(offset), operation_id: operation() }
                }).collect();
                seed_history(owner.client(), services.affiliation(), account.person_id(), records).await;
                let snapshot = "SELECT jsonb_agg(to_jsonb(affiliation) ORDER BY created_at)::text FROM center.affiliations affiliation";
                let before: String = owner.client().query_one(snapshot, &[]).await.expect("independent original bytes").get(0);
                for (page, size, length) in [(1, 1, 1), (2, 1, 1), (1, 20, 20), (1, 100, 100), (2, 100, 1), (3, 100, 0), (i64::MAX, 1, 0)] {
                    let list = services.affiliation().history(&actor, Pagination::new(page, size).expect("valid page")).await.expect("qualified page");
                    assert_eq!(list.count(), 101); assert_eq!(list.affiliations().len(), length);
                    for (index, value) in list.affiliations().iter().enumerate() {
                        let offset = (page-1)*size+i64::try_from(index).expect("page index");
                        assert_eq!(value.created_at, created_at-chrono::Duration::microseconds(offset));
                        assert_eq!(value.value, if offset == 0 { Code::CommunistParty } else { Code::Unaffiliated });
                    }
                }
                assert_eq!(owner.client().query_one(snapshot, &[]).await.expect("unchanged exact facts").get::<_,String>(0), before);
                owner.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            }).await.expect("bounded retained pagination");
        });
    }

    #[test]
    fn history_keeps_identity_count_and_page_in_one_snapshot() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = crate::access::role::tests::start_snapshot(authentication::proxied(&database, &proxy), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let observer = database.connections(&database.user).recover().await.expect("observer");
                proxy.arm(Fault::StalledAffiliationCount);
                let service = services.affiliation().clone(); let reader_actor = member.clone();
                let mut reader = tokio::spawn(async move { service.history(&reader_actor, Pagination::new(1, 20).expect("page")).await });
                timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("identity snapshot precedes count");
                // The observed snapshot must survive beyond the production budget before real writes.
                assert!(timeout(Duration::from_secs(4), &mut reader).await.is_err(), "test context keeps the stalled snapshot alive beyond three seconds");
                let original = operation();
                services.affiliation().create(member.clone(), original, input("01")).await.expect("actual concurrent first fill");
                let candidate = services.account().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("pause confirmation");
                let input = serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"})).expect("pause input");
                Box::pin(services.suspension().suspend(manager.clone(), operation(), candidate.identity().id(), input)).await.expect("actual concurrent suspension");
                proxy.release();
                let result = timeout(Duration::from_secs(2), reader).await.expect("snapshot completes").expect("reader task");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("committed first fill").get::<_,i64>(0), 1);
                assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1)", &[&member.session_id()]).await.expect("actual revocation").get::<_,bool>(0));
                assert!(matches!(result, Err(Error::Unauthenticated)), "anchor movement retries the whole snapshot, including current authority");
                let latest = services.affiliation().history_by_person(&manager, candidate.identity().person_id(), Pagination::new(1, 20).expect("page")).await.expect("fresh authorized snapshot");
                assert_eq!(latest.count(), 1); assert_eq!(latest.affiliations()[0].value(), Code::CommunistParty);
                assert!(matches!(services.affiliation().history(&member, Pagination::new(1, 20).expect("next page")).await, Err(Error::Unauthenticated))); observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded snapshot interleaving");
        }));
    }

    #[test]
    fn history_rejects_damaged_facts_without_returning_partial_pages() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["expired", "tag", "key", "future", "missing_operator", "person"] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let account = services.current().read(&actor).await.expect("person");
                    let original = operation();
                    services.affiliation().create(actor.clone(), original, input("01")).await.expect("first fill");
                    let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                    let created_at: DateTime<Utc> = owner.client().query_one("SELECT created_at FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("original time").get(0);
                    let historical = Id::allocate();
                    seed_history(owner.client(), services.affiliation(), account.person_id(), vec![affiliation::database::Data { id: historical, code: Code::Unaffiliated, operator_id: account.revision_id(), operator_person: account.person_id(), created_at: created_at-chrono::Duration::microseconds(1), operation_id: operation() }]).await;
                    if case == "expired" {
                        let expires_at: DateTime<Utc> = owner.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1", &[&actor.session_id()]).await.expect("original session expiry").get(0);
                        let elapsed = expires_at - database.now().await + chrono::TimeDelta::seconds(1);
                        crate::registration::tests::age_facts(&database, &services, elapsed).await;
                    }
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                    let sql = match case {
                        "expired" => "UPDATE center.affiliations SET value=set_byte(value,0,2)",
                        "tag" => "UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38) # 1) WHERE created_at=(SELECT min(created_at) FROM center.affiliations)",
                        "key" => "UPDATE center.affiliations SET value=set_byte(value,8,7) WHERE created_at=(SELECT min(created_at) FROM center.affiliations)",
                        "future" => "UPDATE center.affiliations SET created_at=created_at+interval '1 day'",
                        "missing_operator" => "UPDATE center.affiliations SET operator_id=gen_random_uuid() WHERE created_at=(SELECT min(created_at) FROM center.affiliations)",
                        "person" => "DELETE FROM identity.persons",
                        _ => unreachable!("fixed cases"),
                    };
                    owner.client().batch_execute(sql).await.expect("selected damage");
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let snapshot = "SELECT jsonb_agg(to_jsonb(affiliation) ORDER BY created_at)::text FROM center.affiliations affiliation";
                    let before: String = owner.client().query_one(snapshot, &[]).await.expect("independent damaged bytes").get(0);
                    let error = services.affiliation().history(&actor, Pagination::new(1, 20).expect("page")).await.err().expect("entire page withheld");
                    assert_eq!(matches!(error, Error::Unauthenticated), case == "expired", "{case}: {error:?}");
                    assert_eq!(error.is_corrupt(), case != "expired", "{case}: {error:?}");
                    assert_eq!(services.ready().await.is_ok(), case == "expired");
                    assert_eq!(owner.client().query_one(snapshot, &[]).await.expect("failed read preserves facts").get::<_,String>(0), before);
                    for private in ["13900000000", "Fixture Operator", "SELECT ", "UPDATE "] { assert!(!format!("{error:?}").contains(private)); }
                    owner.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("history damage case {case}: {error}"));
            }
        }));
    }

    #[test]
    fn history_releases_failed_reads_and_withholds_unconfirmed_pages() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["cancel", "timeout", "count", "page", "commit", "rollback"] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let original = operation();
                    services.affiliation().create(actor.clone(), original, input("01")).await.expect("first fill");
                    let owner = database.connections("postgres").recover().await.expect("independent observer");
                    let snapshot = "SELECT jsonb_agg(to_jsonb(affiliation) ORDER BY created_at)::text FROM center.affiliations affiliation";
                    let before: String = owner.client().query_one(snapshot, &[]).await.expect("original bytes").get(0);
                    if case == "rollback" {
                        services.revocation().revoke(actor.clone(), operation(), crate::identity::revocation::Scope::Current).await.expect("actual identity loss");
                    }
                    let second = context.connections().lease().await.expect("hold other pool slot");
                    let first = context.connections().lease().await.expect("pin read connection");
                    let pid: i32 = first.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("read backend identity").get(0);
                    first.release();
                    proxy.arm(match case { "page" | "timeout" => Fault::StalledAffiliationPage, "rollback" => Fault::RejectedRollback, _ => Fault::StalledAffiliationCount });
                    let service = services.affiliation().clone(); let reader_actor = actor.clone();
                    let reader = tokio::spawn(async move { service.history(&reader_actor, Pagination::new(1, 20).expect("page")).await });
                    if case != "rollback" {
                        timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual list query intercepted");
                    }
                    if matches!(case, "count" | "page") {
                        owner.client().batch_execute(&format!("REVOKE SELECT ON center.affiliations FROM {}", database.user)).await.expect("isolated query denial");
                    }
                    if case == "commit" { proxy.arm(Fault::LostReadCommit); }
                    if !matches!(case, "cancel" | "timeout" | "rollback") { proxy.release(); }
                    if case == "cancel" {
                        reader.abort(); assert!(reader.await.err().expect("cancelled read").is_cancelled());
                    } else {
                        let error = timeout(Duration::from_secs(4), reader).await.expect("bounded read failure").expect("reader task").err().expect("no partial page");
                        if case == "rollback" {
                            assert!(matches!(error, Error::Unauthenticated), "{error:?}");
                        } else {
                            let Error::Unavailable(fault) = &error else { panic!("{case}: {error:?}"); };
                            match case {
                                "timeout" => assert!(matches!(fault.kind(), FaultKind::Timeout(_)), "{error:?}"),
                                "count" | "page" => assert!(matches!(fault.kind(), FaultKind::History(history::Error::Query(query)) if query.code() == Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE))),
                                "commit" => assert!(matches!(fault.kind(), FaultKind::Query(_)) && fault.stage() == Stage::Read, "{error:?}"),
                                _ => unreachable!("non-cancelled cases"),
                            }
                        }
                    }
                    proxy.release();
                        timeout(Duration::from_secs(3), async {
                            while owner.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1)", &[&pid]).await.expect("original backend observation").get::<_,bool>(0) { tokio::task::yield_now().await; }
                        }).await.expect("original backend ended before fixture cleanup");
                    second.release();
                    if matches!(case, "count" | "page") {
                        owner.client().batch_execute(&format!("GRANT SELECT ON center.affiliations TO {}", database.user)).await.expect("restore isolated grant");
                    }
                    services.ready().await.expect("ordinary failure preserves admission");
                    if case != "rollback" {
                        assert_eq!(services.affiliation().history(&actor, Pagination::new(1, 20).expect("page")).await.expect("fresh read after ordinary failure").count(), 1);
                    }
                    assert_eq!(owner.client().query_one(snapshot, &[]).await.expect("read failures preserve exact facts").get::<_,String>(0), before);
                    services.close(); services.drain().await.expect("drained"); let _ = context.stop().await;
                    timeout(Duration::from_secs(3), async {
                        while owner.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND usename=$1)", &[&database.user]).await.expect("runtime backend observation").get::<_,bool>(0) { tokio::task::yield_now().await; }
                    }).await.expect("all runtime backends ended before fixture cleanup");
                    owner.terminate().await.expect("observer ended"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("history failure case {case}: {error}"));
            }
        }));
    }

    #[test]
    fn accepts_only_the_thirteen_exact_codes_and_redacts_diagnostics() {
        for number in 1..=13 {
            let value = format!("{number:02}");
            let input = input(&value);
            assert_eq!(
                serde_json::to_value(input.affiliation).expect("canonical output"),
                value
            );
            assert_eq!(format!("{:?}", input.affiliation), "Code([redacted])");
        }
        for value in [
            json!({}),
            json!({"affiliation":null}),
            json!({"affiliation":1}),
            json!({"affiliation":["01"]}),
            json!({"affiliation":"01","extra":true}),
        ] {
            assert!(serde_json::from_value::<Input>(value).is_err());
        }
        for value in [
            "",
            "1",
            "001",
            "00",
            "14",
            " 01",
            "01 ",
            "０１",
            "群众",
            "sensitive-marker",
        ] {
            let error = serde_json::from_value::<Input>(json!({"affiliation":value}))
                .err()
                .expect("invalid value");
            assert!(!format!("{error:?}").contains("sensitive-marker"));
        }
        let schema =
            serde_json::to_value(<Code as utoipa::PartialSchema>::schema()).expect("schema");
        assert_eq!(
            schema["enum"],
            json!([
                "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12", "13"
            ])
        );
    }

    #[test]
    fn authenticates_independent_ciphertext_and_retained_keys_with_both_identity_parts() {
        let old = crate::keys::Id::new(0).expect("old");
        let active = crate::keys::Id::new(1).expect("active");
        let entry = || {
            Entry::new(
                old,
                Material(Zeroizing::new(std::array::from_fn(|index| {
                    u8::try_from(index).expect("32 bytes")
                }))),
            )
        };
        let retained = Keys::new(crate::keys::Derived::fixed(
            active,
            vec![
                entry(),
                Entry::new(active, Material(Zeroizing::new([91; 32]))),
            ],
        ));
        let person: person::Id =
            serde_json::from_value(json!("00000000-0000-4000-8000-000000000001")).expect("person");
        let id = Id(Uuid::parse_str("00000000-0000-4000-8000-000000000002").expect("row"));
        // Python cryptography AESGCMSIV, nonce 0..11, LP32(domain, header, person||row, field).
        let bytes = hex::decode(
            "010000000000000000000102030405060708090a0bcc444c0cdfecd927ab78243c8ea478c8d062",
        )
        .expect("independent vector");
        let sealed = Sealed(Envelope::new(bytes.clone()).expect("envelope"));
        assert_eq!(
            envelope::open(&retained, person, id, &sealed).expect("authenticated retained value"),
            Code::CommunistParty
        );
        assert!(envelope::open(&retained, person::Id::allocate(), id, &sealed).is_err());
        assert!(envelope::open(&retained, person, Id::allocate(), &sealed).is_err());
        let retired = Keys::new(crate::keys::Derived::fixed(
            active,
            vec![Entry::new(active, Material(Zeroizing::new([91; 32])))],
        ));
        assert!(matches!(
            envelope::open(&retired, person, id, &sealed),
            Err(history::Error::Encryption(
                crate::kernel::keys::Error::Missing(_)
            ))
        ));
        for offset in [9, 21, 38] {
            let mut corrupted = bytes.clone();
            corrupted[offset] ^= 1;
            assert!(
                envelope::open(
                    &retained,
                    person,
                    id,
                    &Sealed(Envelope::new(corrupted).expect("valid envelope shape"))
                )
                .is_err()
            );
        }
        let first = envelope::seal(&retained, person, id, Code::Unaffiliated).expect("seal");
        let second = envelope::seal(&retained, person, id, Code::Unaffiliated).expect("fresh seal");
        assert_ne!(first.0.as_ref(), second.0.as_ref());
        assert_eq!(first.0.as_ref().len(), 39);
        assert_eq!(
            envelope::open(&retained, person, id, &first).expect("active value"),
            Code::Unaffiliated
        );
    }

    #[test]
    fn first_fill_preserves_permanent_facts_replay_and_account_independence() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let account = services.current().read(&member).await.expect("ordinary account");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND person_id=$1) AND NOT EXISTS(SELECT 1 FROM access.holders holder JOIN access.permissions permission ON permission.role_id=holder.role_id WHERE holder.person_id=$1)", &[&account.person_id()]).await.expect("no Workforce membership or management authority").get::<_,bool>(0));
                let original = operation();
                services.affiliation().create(member.clone(), original, input("01")).await.expect("voluntary first fill");
                let row = observer.client().query_one("SELECT id, person_id, operator_id, created_at, value, (SELECT sequence FROM identity.persons WHERE id=$2) AS sequence FROM center.affiliations WHERE operation_id=$1", &[&original,&account.person_id()]).await.expect("independent permanent facts before replay");
                assert_eq!(row.get::<_,person::Id>("person_id"), account.person_id());
                assert_eq!(row.get::<_,person::revision::Id>("operator_id"), account.revision_id());
                assert_eq!(row.get::<_,i64>("sequence"), 1, "Center does not allocate a personal record");
                assert!(row.get::<_,DateTime<Utc>>("created_at") <= context.sample().await.expect("trusted time"));
                let sealed: Vec<u8> = row.get("value"); assert_eq!(sealed.len(),39); assert_ne!(sealed,b"01");
                services.affiliation().create(member.clone(), original, input("01")).await.expect("same original");
                assert!(matches!(services.affiliation().create(member.clone(), original, input("13")).await,Err(Error::Mismatch)));
                assert!(matches!(services.affiliation().create(member.clone(), operation(), input("01")).await,Err(Error::Rejected(Rejection::Conflict))));
                for code in 1..=13 {
                    assert!(matches!(services.affiliation().create(administrator.clone(), original, input(&format!("{code:02}"))).await,Err(Error::Mismatch)));
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM center.affiliations",&[]).await.expect("one immutable row").get::<_,i64>(0),1);
                services.correction().correct(administrator, operation(), serde_json::from_value(json!({"target":{"personId":account.person_id(),"revisionId":account.revision_id()},"name":"Corrected Person","idNumber":"00000020000229007X"})).expect("actual correction input")).await.expect("later real-name correction");
                services.affiliation().create(member.clone(), original, input("01")).await.expect("original does not rebind to the new revision");
                let account = services.current().read(&member).await.expect("corrected current identity");
                let cancelled=Box::pin(services.cancellation().cancel(member.clone(),operation(),serde_json::from_value(json!({"accountId":account.id(),"personId":account.person_id(),"revisionId":account.revision_id()})).expect("confirmed cancellation"))).await.expect("actual self cancellation");
                assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM center.affiliations WHERE operation_id=$1 AND value=$2) AND EXISTS(SELECT 1 FROM identity.accounts WHERE id=$3 AND person_id=$4 AND cancellation_id=$5 AND mobile IS NULL AND id_number IS NULL AND suspension_id IS NULL)", &[&original,&sealed,&account.id(),&account.person_id(),&cancelled.id()]).await.expect("cancellation preserves Center value and the exact Account tombstone").get::<_,bool>(0));
                assert!(matches!(services.affiliation().create(member, original, input("01")).await,Err(Error::Unauthenticated)));
                let replacement = register(&database, &services, &mut supplier).await;
                let replacement_account = services.current().read(&replacement).await.expect("new account");
                assert_ne!(replacement_account.person_id(), account.person_id(), "same mobile does not inherit the cancelled person's data");
                services.affiliation().create(replacement, operation(), input("13")).await.expect("new person may fill independently");
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                let (context,services) = start(database.connections(&database.user),&supplier).await;
                services.ready().await.expect("retained history after account cancellation");
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("first affiliation lifecycle deadline");
        }));
    }

    #[test]
    fn current_read_belongs_to_the_person_across_correction_and_account_replacement() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let account = services.current().read(&member).await.expect("ordinary account");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL AND person_id=$1) AND NOT EXISTS(SELECT 1 FROM access.holders holder JOIN access.permissions permission ON permission.role_id=holder.role_id WHERE holder.person_id=$1)", &[&account.person_id()]).await.expect("no Workforce membership or management authority").get::<_,bool>(0));
                assert!(matches!(services.affiliation().read(&member).await, Err(Error::Rejected(Rejection::Unset))));
                let original = operation();
                services.affiliation().create(member.clone(), original, input("01")).await.expect("actual first fill");
                let row = observer.client().query_one("SELECT value,created_at FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("independent original facts");
                let value: Vec<u8> = row.get("value"); let created_at: DateTime<Utc> = row.get("created_at");
                let affiliation = services.affiliation().read(&member).await.expect("ordinary person's current value");
                assert_eq!(affiliation.value, Code::CommunistParty); assert_eq!(affiliation.created_at, created_at);
                assert!(matches!(services.affiliation().read(&administrator).await, Err(Error::Rejected(Rejection::Unset))), "another person's value remains private");
                Box::pin(services.correction().correct(administrator, operation(), serde_json::from_value(json!({"target":{"personId":account.person_id(),"revisionId":account.revision_id()},"name":"Corrected Person","idNumber":"00000020000229007X"})).expect("actual correction input"))).await.expect("real-name correction");
                let affiliation = services.affiliation().read(&member).await.expect("same person's original value after correction");
                assert_eq!(affiliation.value, Code::CommunistParty); assert_eq!(affiliation.created_at, created_at);
                let account = services.current().read(&member).await.expect("corrected current identity");
                Box::pin(services.cancellation().cancel(member.clone(),operation(),serde_json::from_value(json!({"accountId":account.id(),"personId":account.person_id(),"revisionId":account.revision_id()})).expect("confirmed cancellation"))).await.expect("actual self cancellation");
                assert!(matches!(services.affiliation().read(&member).await, Err(Error::Unauthenticated)));
                let replacement = register(&database, &services, &mut supplier).await;
                let replacement_account = services.current().read(&replacement).await.expect("same mobile with a new person");
                assert_ne!(replacement_account.person_id(), account.person_id());
                assert!(matches!(services.affiliation().read(&replacement).await, Err(Error::Rejected(Rejection::Unset))));
                let row = observer.client().query_one("SELECT count(*),bool_and(operation_id=$1 AND person_id=$2 AND value=$3 AND created_at=$4) FROM center.affiliations", &[&original,&account.person_id(),&value,&created_at]).await.expect("all reads and account changes preserve original facts");
                assert_eq!(row.get::<_,i64>(0),1); assert!(row.get::<_,bool>(1));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("person-owned current affiliation budget");
        }));
    }

    #[test]
    fn current_read_uses_effective_time_and_preserves_all_retained_values() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let account = services.current().read(&actor).await.expect("person");
                let original = operation();
                services.affiliation().create(actor.clone(), original, input("01")).await.expect("actual first fill");
                let owner = database.connections("postgres").recover().await.expect("retained fixture owner");
                let row = owner.client().query_one("SELECT created_at, value FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("independent latest fact");
                let created_at: DateTime<Utc> = row.get("created_at");
                let historical = Id(Uuid::from_u128(u128::MAX));
                // Isolated retained material proves read ordering, not a delivered correction path.
                seed_history(owner.client(), services.affiliation(), account.person_id(), vec![affiliation::database::Data { id: historical, code: Code::Unaffiliated, operator_id: account.revision_id(), operator_person: account.person_id(), created_at: created_at-chrono::Duration::microseconds(1), operation_id: operation() }]).await;
                let original_value: Vec<u8> = owner.client().query_one("SELECT value FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("seeded current value").get(0);
                let value: Vec<u8> = owner.client().query_one("SELECT value FROM center.affiliations WHERE id=$1", &[&historical]).await.expect("retained earlier value").get(0);
                let affiliation = services.affiliation().read(&actor).await.expect("current value");
                assert_eq!(affiliation.value, Code::CommunistParty);
                assert_eq!(affiliation.created_at, created_at);
                let row = owner.client().query_one("SELECT count(*), bool_and(CASE WHEN operation_id=$1 THEN value=$2 AND created_at=$3 ELSE id=$4 AND value=$5 END) FROM center.affiliations WHERE person_id=$6", &[&original,&original_value,&created_at,&historical,&value,&account.person_id()]).await.expect("independent unchanged retained facts");
                assert_eq!(row.get::<_,i64>(0), 2);
                assert!(row.get::<_,bool>(1));
                owner.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            }).await.expect("current retained value budget");
        });
    }

    #[test]
    fn current_read_confirms_identity_before_value_decoding_and_closes_on_damage() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["expired", "revoked", "removed", "tag", "key", "future", "person", "account", "operator"] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let original = operation();
                    services.affiliation().create(actor.clone(), original, input("01")).await.expect("actual first fill");
                    services.affiliation().read(&actor).await.expect("healthy read before the fault");
                    if case == "revoked" {
                        services.revocation().revoke(actor.clone(), operation(), crate::identity::revocation::Scope::Current).await.expect("actual revocation after Actor construction");
                    }
                    let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                    if case == "expired" {
                        let expires_at: DateTime<Utc> = owner.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1", &[&actor.session_id()]).await.expect("original session expiry").get(0);
                        let elapsed = expires_at - database.now().await + chrono::TimeDelta::seconds(1);
                        crate::registration::tests::age_facts(&database, &services, elapsed).await;
                    }
                    if case == "operator" {
                        owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated operator fault");
                        owner.client().execute("UPDATE center.affiliations SET operator_id=$1 WHERE operation_id=$2", &[&person::revision::Id::allocate(),&original]).await.expect("valid ciphertext paired with a missing operator");
                        owner.client().batch_execute("COMMIT").await.expect("operator fault committed");
                    } else {
                        let damage = match case {
                            "expired" => "UPDATE center.affiliations SET value=set_byte(value,0,2)",
                            "revoked" => "UPDATE center.affiliations SET value=set_byte(value,0,2)",
                            "removed" => "DELETE FROM identity.sessions; UPDATE center.affiliations SET value=set_byte(value,0,2)",
                            "tag" => "UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38) # 1)",
                            "key" => "UPDATE center.affiliations SET value=set_byte(value,8,7)",
                            "future" => "UPDATE center.affiliations SET created_at=created_at+interval '1 day'",
                            "person" => "DELETE FROM identity.persons",
                            "account" => "DELETE FROM identity.accounts",
                            _ => unreachable!("fixed case list"),
                        };
                        owner.client().batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; {damage}; COMMIT")).await.expect("isolated post-authentication fault");
                    }
                    // Deleting an anchored Session is damage, unlike expiry or revocation.
                    let invalid = matches!(case, "expired" | "revoked");
                    let error = services.affiliation().read(&actor).await.err().expect("changed facts prevent disclosure");
                    assert_eq!(matches!(error, Error::Unauthenticated), invalid, "{case}: {error:?}");
                    assert_eq!(error.is_corrupt(), !invalid, "{case}: {error:?}");
                    assert_eq!(services.ready().await.is_ok(), invalid, "{case}");
                    if !invalid {
                        assert!(matches!(services.affiliation().create(actor, operation(), input("13")).await, Err(Error::Unavailable(fault)) if matches!(fault.kind(), FaultKind::Closed)));
                    }
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("failed read preserves the retained row").get::<_,i64>(0), 1);
                    for private in ["13900000000", "Fixture Operator", "SELECT ", "UPDATE "] {
                        assert!(!format!("{error:?}").contains(private));
                    }
                    owner.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("current affiliation case {case}: {error}"));
            }
        }));
    }

    #[test]
    fn current_read_discards_cancelled_and_timed_out_queries_without_closing_admission() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for cancel in [true, false] {
                timeout(Duration::from_secs(40), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let original = operation();
                    services.affiliation().create(actor.clone(), original, input("01")).await.expect("first fill");
                    let owner = database.connections("postgres").recover().await.expect("independent observer");
                    let value: Vec<u8> = owner.client().query_one("SELECT value FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("original bytes").get(0);
                    let first = context.connections().lease().await.expect("selected pooled session");
                    let second = context.connections().lease().await.expect("hold the other pool slot");
                    let pid: i32 = first.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("selected backend").get(0);
                    first.release();
                    proxy.arm(Fault::StalledAffiliationRead);
                    let service = services.affiliation().clone();
                    let reader_actor = actor.clone();
                    let reader = tokio::spawn(async move { service.read(&reader_actor).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("business query held after the Actor and initial sample");
                    if cancel {
                        reader.abort();
                        assert!(reader.await.err().expect("cancelled read").is_cancelled());
                    } else {
                        assert!(matches!(timeout(Duration::from_secs(15), reader).await.expect("query deadline and cleanup").expect("reader task"), Err(Error::Unavailable(fault)) if matches!(fault.kind(), FaultKind::Timeout(_))));
                    }
                    proxy.release();
                    timeout(Duration::from_secs(3), async {
                        while owner.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1)", &[&pid]).await.expect("original backend observation").get::<_,bool>(0) {
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("discarded backend ended before fixture cleanup");
                    second.release();
                    services.ready().await.expect("ordinary query interruption retains admission");
                    assert_eq!(services.affiliation().read(&actor).await.expect("fresh read after interruption").value, Code::CommunistParty);
                    let row = owner.client().query_one("SELECT count(*),bool_and(operation_id=$1 AND value=$2) FROM center.affiliations", &[&original,&value]).await.expect("independent facts after retry");
                    assert_eq!(row.get::<_,i64>(0),1); assert!(row.get::<_,bool>(1));
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("all local drivers stopped");
                    assert!(context.connections().lease().await.is_err());
                    timeout(Duration::from_secs(3), async {
                        while owner.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND usename=$1)", &[&database.user]).await.expect("runtime backend observation").get::<_,bool>(0) {
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("runtime backends ended before fixture cleanup");
                    owner.terminate().await.expect("observer ended"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                }).await.expect("bounded interrupted affiliation read");
            }
        });
    }

    #[test]
    fn current_read_rechecks_expiry_and_high_water_after_waiting_for_its_connection() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for rewind in [false, true] {
                timeout(Duration::from_secs(25), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    services.affiliation().create(actor.clone(), operation(), input("01")).await.expect("first fill");
                    let owner = database.connections("postgres").recover().await.expect("time observer");
                    let expires_at: DateTime<Utc> = owner.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1", &[&actor.session_id()]).await.expect("original session expiry").get(0);
                    let first = context.connections().lease().await.expect("first pool slot");
                    let second = context.connections().lease().await.expect("second pool slot");
                    let baseline = context.now(first.client()).await.expect("initial high water");
                    assert!(expires_at > baseline, "session is valid before the pool wait");
                    let service = services.affiliation().clone();
                    let (waiting, queued) = tokio::sync::oneshot::channel();
                    let reader = tokio::spawn(async move {
                        use std::future::Future as _;
                        let mut result = Box::pin(service.read(&actor));
                        std::future::poll_fn(|cx| {
                            assert!(result.as_mut().poll(cx).is_pending(), "both pool slots force this read to wait");
                            std::task::Poll::Ready(())
                        }).await;
                        waiting.send(()).expect("reader poll observed");
                        result.await
                    });
                    timeout(Duration::from_secs(2), queued).await.expect("read reached its pool wait").expect("reader notified");
                    assert!(!reader.is_finished());
                    if rewind {
                        proxy.arm(Fault::RewoundCurrent);
                        first.release(); second.release();
                        timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("decisive affiliation snapshot");
                        proxy.release();
                    } else {
                        let elapsed = expires_at - database.now().await + chrono::TimeDelta::seconds(1);
                        crate::registration::tests::age_facts(&database, &services, elapsed).await;
                        assert!(!reader.is_finished(), "expiry is committed while the reader still waits");
                        first.release(); second.release();
                    }
                    let result = timeout(Duration::from_secs(3), reader).await.expect("reader ended").expect("read task");
                    if rewind {
                        assert!(matches!(result, Err(Error::Unavailable(fault)) if matches!(fault.kind(), FaultKind::Clock(crate::runtime::Error::ClockInvalid))));
                        assert!(services.ready().await.is_err());
                    } else {
                        assert!(matches!(result, Err(Error::Unauthenticated)));
                        services.ready().await.expect("ordinary expiry retains admission");
                    }
                    owner.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                }).await.expect("pool wait and trusted-time budget");
            }
        });
    }

    #[test]
    fn current_read_uses_snapshot_time_and_preserves_ordinary_query_failures() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for case in ["cancel", "success", "query"] {
                timeout(Duration::from_secs(25), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let original = operation();
                    services.affiliation().create(actor.clone(), original, input("01")).await.expect("first fill");
                    let owner = database.connections("postgres").recover().await.expect("independent observer");
                    let value: Vec<u8> = owner.client().query_one("SELECT value FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("original bytes").get(0);
                    let instance = context.clock_session().await;
                    let before = proxy.time_queries();
                    proxy.arm(Fault::StalledAffiliationRead);
                    let service = services.affiliation().clone();
                    let reader_actor = actor.clone();
                    let reader = tokio::spawn(async move { service.read(&reader_actor).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("read query after initial sampling");
                    if case == "query" {
                        owner.client().batch_execute(&format!("REVOKE SELECT ON center.affiliations FROM {}", database.user)).await.expect("ordinary business query failure");
                    }
                    if case == "cancel" {
                        reader.abort(); assert!(reader.await.err().expect("cancelled reader").is_cancelled());
                    } else {
                        proxy.release();
                        let result = timeout(Duration::from_secs(3), reader).await.expect("read finished").expect("reader task");
                        if case == "query" {
                            let Error::Unavailable(fault) = result.err().expect("query failed") else { panic!("unavailable query"); };
                            assert!(matches!(fault.kind(), FaultKind::History(history::Error::Query(_))), "{fault:?}");
                            assert!(!fault.later().any(|(_, kind)| matches!(kind, FaultKind::Clock(_))));
                            owner.client().batch_execute(&format!("GRANT SELECT ON center.affiliations TO {}", database.user)).await.expect("restore fixture grant");
                        } else {
                            result.expect("complete snapshot result");
                        }
                    }
                    proxy.release();
                    assert_eq!(proxy.time_queries() - before, 1);
                    services.affiliation().read(&actor).await.expect("a fresh affiliation read");
                    services.ready().await.expect("ordinary query failures preserve admission");
                    drop(instance);
                    let row = owner.client().query_one("SELECT count(*),bool_and(operation_id=$1 AND value=$2) FROM center.affiliations", &[&original,&value]).await.expect("business facts after failed confirmation");
                    assert_eq!(row.get::<_,i64>(0),1); assert!(row.get::<_,bool>(1));
                    owner.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); let _ = context.stop().await; proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                }).await.unwrap_or_else(|error| panic!("final sampling case {case}: {error}"));
            }
        });
    }

    #[test]
    fn concurrent_first_fills_wait_for_the_person_and_commit_only_one_fact() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database,&proxy), &supplier).await;
                let actor = actor(&database,&services,&mut supplier).await;
                let first_id = operation(); let second_id = operation();
                proxy.arm(Fault::DelayedCommit);
                let service = services.affiliation().clone(); let first_actor = actor.clone();
                let first = tokio::spawn(async move { service.create(first_actor, first_id, input("01")).await });
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("first write awaits commit");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let pid = proxy.intercepted().await;
                let service = services.affiliation().clone();
                let second = tokio::spawn(async move { service.create(actor,second_id,input("13")).await });
                timeout(Duration::from_secs(3), async {
                    loop {
                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND query LIKE 'SELECT id FROM identity.persons WHERE id%' AND wait_event_type='Lock')", &[&pid]).await.expect("second fill waits on the same person").get::<_,bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("observed person-lock wait");
                proxy.release();
                first.await.expect("first waiter").expect("original success");
                assert!(matches!(second.await.expect("second waiter"),Err(Error::Rejected(Rejection::Conflict))));
                let row = observer.client().query_one("SELECT count(*), bool_and(operation_id=$1) FROM center.affiliations", &[&first_id]).await.expect("one committed operation");
                assert_eq!(row.get::<_,i64>(0),1); assert!(row.get::<_,bool>(1));
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop"); proxy.stop().await; supplier.stop().await;
            })).await.expect("concurrent fill deadline");
        }));
    }

    #[test]
    fn an_unknown_candidate_is_settled_in_process_by_replay_or_the_next_writer() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for replay_first in [false, true] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let administrator = actor(&database, &services, &mut supplier).await;
                    let member = register(&database, &services, &mut supplier).await;
                    let person = services.current().read(&member).await.expect("target person");
                    let original = operation();
                    proxy.arm(Fault::UnknownCommit);
                    let service = services.affiliation().clone(); let accepted = member.clone();
                    let first = tokio::spawn(async move { service.create(accepted, original, input("01")).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original commit held");
                    assert!(matches!(timeout(Duration::from_secs(3), first).await.expect("failed independent recovery").expect("waiter"), Err(Error::Unknown(_))));
                    services.ready().await.expect("one unknown write does not close unrelated admission");
                    assert!(matches!(services.affiliation().read(&administrator).await, Err(Error::Rejected(Rejection::Unset))));
                    proxy.release();
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    observer.client().batch_execute("BEGIN").await.expect("observe original termination");
                    observer.client().query_one("SELECT id FROM identity.persons WHERE id=$1 FOR UPDATE", &[&person.person_id()]).await.expect("late original transaction released its object lock");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("durable result before any replay").get::<_,i64>(0), 1);
                    observer.client().batch_execute("COMMIT").await.expect("end observation");
                    if replay_first {
                        services.affiliation().create(member.clone(), original, input("01")).await.expect("original replay settles the matching candidate");
                    }
                    let correction = serde_json::from_value(json!({
                        "target": {"type":"account","accountId":person.id(),"personId":person.person_id(),"revisionId":person.revision_id()},
                        "affiliation":"13",
                    })).expect("confirmed correction");
                    services.affiliation().correct(administrator, operation(), person.person_id(), correction).await.expect("a new locked writer resolves any remaining uncertainty and advances");
                    services.affiliation().create(member.clone(), original, input("01")).await.expect("older replay cannot roll the newer anchor back");
                    let list = services.affiliation().history(&member, Pagination::new(1,20).expect("page")).await.expect("latest anchored history");
                    assert_eq!(list.count(), 2);
                    assert_eq!(list.affiliations()[0].value, Code::Unaffiliated);
                    assert_eq!(list.affiliations()[1].value, Code::CommunistParty);
                    assert_eq!(observer.client().query_one("SELECT sequence,version FROM center.profiles WHERE id=$1", &[&person.person_id()]).await.expect("independent counters").get::<_,i64>(0), 2);
                    services.ready().await.expect("settled runtime remains available");
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await; supplier.stop().await;
                })).await.expect("bounded uncertain-state recovery");
            }
        }));
    }

    #[test]
    fn commit_faults_recover_once_and_cancelling_the_waiter_does_not_end_the_work() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [
                Fault::LostCommit,
                Fault::RolledBack,
                Fault::UnknownCommit,
                Fault::ReadCommitTermination,
                Fault::DelayedCommit,
            ] {
                timeout(
                    Duration::from_secs(30),
                    Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) =
                            start(authentication::proxied(&database, &proxy), &supplier).await;
                        let actor = actor(&database, &services, &mut supplier).await;
                        let account = services
                            .current()
                            .read(&actor)
                            .await
                            .expect("original person");
                        let original = operation();
                        let cancelled = matches!(fault, Fault::DelayedCommit);
                        let rolled_back = matches!(fault, Fault::RolledBack);
                        let unknown = matches!(fault, Fault::UnknownCommit);
                        let terminated = matches!(fault, Fault::ReadCommitTermination);
                        proxy.arm(if terminated {
                            Fault::DelayedCommit
                        } else {
                            fault
                        });
                        let service = services.affiliation().clone();
                        let original_actor = actor.clone();
                        let waiter = tokio::spawn(async move {
                            service.create(original_actor, original, input("01")).await
                        });
                        timeout(Duration::from_secs(3), proxy.intercepted())
                            .await
                            .expect("actual business commit fault");
                        if terminated {
                            proxy.arm(Fault::ReadCommitTermination);
                        }
                        if cancelled {
                            waiter.abort();
                            let _ = waiter.await;
                            services.close();
                            assert!(
                                timeout(Duration::from_millis(50), services.drain())
                                    .await
                                    .is_err(),
                                "waiter cancellation does not release the accepted work"
                            );
                            proxy.release();
                            services.drain().await.expect("work finishes during drain");
                        } else {
                            proxy.release();
                            let result = timeout(Duration::from_secs(15), waiter)
                                .await
                                .expect("bounded work")
                                .expect("waiter");
                            if rolled_back {
                                assert!(matches!(result, Err(Error::Unavailable(fault)) if fault.stage() == Stage::Commit));
                            } else if unknown {
                                assert!(matches!(result, Err(Error::Unknown(_))));
                            } else {
                                result.expect("complete permanent success");
                            }
                            services.close();
                            services.drain().await.expect("drain");
                        }
                        context.stop().await.expect("stop");
                        let observer = database
                            .connections(&database.user)
                            .recover()
                            .await
                            .expect("observer");
                        observer
                            .client()
                            .batch_execute("BEGIN")
                            .await
                            .expect("observe original backend termination");
                        observer
                            .client()
                            .query_one(
                                "SELECT id FROM identity.persons WHERE id=$1 FOR UPDATE",
                                &[&account.person_id()],
                            )
                            .await
                            .expect("original backend released its lock");
                        assert_eq!(
                            observer
                                .client()
                                .query_one(
                                    "SELECT count(*) FROM center.affiliations WHERE operation_id=$1",
                                    &[&original]
                                )
                                .await
                                .expect("facts before replay")
                                .get::<_, i64>(0),
                            i64::from(!rolled_back)
                        );
                        observer
                            .client()
                            .batch_execute("COMMIT")
                            .await
                            .expect("observer transaction ended");
                        proxy.stop().await;
                        let (context, services) =
                            start(database.connections(&database.user), &supplier).await;
                        services
                            .affiliation()
                            .create(actor.clone(), original, input("01"))
                            .await
                            .expect("explicit retry after confirmed rollback or original replay");
                        let (same, different) = tokio::join!(
                            services
                                .affiliation()
                                .create(actor.clone(), original, input("01")),
                            services.affiliation().create(actor, original, input("13"))
                        );
                        same.expect("matching waiter");
                        assert!(matches!(different, Err(Error::Mismatch)));
                        assert_eq!(
                            observer
                                .client()
                                .query_one(
                                    "SELECT count(*) FROM center.affiliations WHERE person_id=$1",
                                    &[&account.person_id()]
                                )
                                .await
                                .expect("only one permanent row")
                                .get::<_, i64>(0),
                            1
                        );
                        observer.terminate().await.expect("observer ended");
                        services.close();
                        services.drain().await.expect("drain");
                        context.stop().await.expect("stop");
                        assert!(
                            timeout(Duration::from_millis(30), supplier.request())
                                .await
                                .is_err()
                        );
                        supplier.stop().await;
                    }),
                )
                .await
                .expect("one commit-fault scenario deadline");
            }
        }));
    }

    #[test]
    fn recovery_retains_the_qualified_person_and_rejects_damaged_completion() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["revoked", "ciphertext", "operator"] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database,&proxy), &supplier).await;
                    let actor = actor(&database,&services,&mut supplier).await;
                    let other = register(&database,&services,&mut supplier).await;
                    let other = services.current().read(&other).await.expect("another real person");
                    let original = operation();
                    proxy.arm(Fault::DelayedCommit);
                    let service = services.affiliation().clone(); let accepted = actor.clone();
                    let waiter = tokio::spawn(async move { service.create(accepted,original,input("01")).await });
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("lost original commit response");
                    proxy.arm(Fault::StalledAffiliation);
                    proxy.release();
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("independent recovery reached its original-fact query");
                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM center.affiliations WHERE operation_id=$1",&[&original]).await.expect("committed fact before recovery or replay").get::<_,i64>(0),1);
                    if case != "revoked" {
                        owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated corruption");
                        if case == "ciphertext" {
                            owner.client().execute("UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38) # 1) WHERE operation_id=$1",&[&original]).await.expect("damage authenticated value");
                        } else {
                            owner.client().execute("UPDATE center.affiliations SET operator_id=$2 WHERE operation_id=$1",&[&original,&other.revision_id()]).await.expect("existing revision of a different person");
                        }
                        owner.client().batch_execute("COMMIT").await.expect("corruption precedes recovery read");
                    } else {
                        services.revocation().revoke(actor.clone(),operation(),crate::identity::revocation::Scope::Current).await.expect("actual revocation before recovery read");
                        assert!(owner.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1)",&[&actor.session_id()]).await.expect("revocation committed").get::<_,bool>(0));
                    }
                    proxy.release();
                    let result = waiter.await.expect("accepted waiter");
                    if case != "revoked" {
                        assert!(matches!(result,Err(Error::Unknown(_))));
                        assert!(services.ready().await.is_err(),"damaged completion closes admission");
                    } else {
                        result.expect("later revocation does not erase the accepted completion");
                        assert!(matches!(services.affiliation().create(actor,original,input("01")).await,Err(Error::Unauthenticated)));
                    }
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM center.affiliations",&[]).await.expect("recovery never repeats the write").get::<_,i64>(0),1);
                    owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop"); proxy.stop().await; supplier.stop().await;
                })).await.expect("bounded recovery interleaving");
            }
        }));
    }

    #[test]
    fn runtime_sql_preserves_state_record_pairing_and_immutable_material() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let account = services.current().read(&actor).await.expect("current person");
                let original = operation();
                services.affiliation().create(actor, original, input("01")).await.expect("actual first fill");
                let runtime = database.connections(&database.user).recover().await.expect("runtime SQL");
                let columns: Vec<String> = runtime.client().query("SELECT attname FROM pg_attribute WHERE attrelid='center.affiliations'::regclass AND attnum>0 AND NOT attisdropped ORDER BY attnum", &[]).await.expect("record columns").iter().map(|row| row.get(0)).collect();
                assert_eq!(columns, ["id", "person_id", "value", "operator_id", "created_at", "operation_id", "sequence", "seal", "seal_key_id", "seal_version"]);
                for statement in [
                    "UPDATE center.affiliations SET created_at=created_at",
                    "UPDATE center.affiliations SET seal=seal",
                    "DELETE FROM center.affiliations",
                    "UPDATE center.completions SET input=input",
                    "DELETE FROM center.completions",
                    "DELETE FROM center.profiles",
                ] {
                    assert_eq!(runtime.client().execute(statement, &[]).await.expect_err("runtime cannot mutate permanent records or remove states").code().map(tokio_postgres::error::SqlState::code), Some("42501"));
                }
                for statement in [
                    "UPDATE center.profiles SET sequence=sequence+1,version=version+1",
                    "UPDATE center.profiles SET version=version+1",
                    "UPDATE center.profiles SET sequence=sequence+1",
                ] {
                    runtime.client().batch_execute("BEGIN").await.expect("structural probe");
                    runtime.client().execute(statement, &[]).await.expect("deferred state write");
                    assert_eq!(runtime.client().batch_execute("SET CONSTRAINTS center.profiles_records_check IMMEDIATE").await.expect_err("each advance requires its new contiguous records").code().map(tokio_postgres::error::SqlState::code), Some("23514"));
                    runtime.client().batch_execute("ROLLBACK").await.expect("discard structural probe");
                }
                let inventory = authentication::inventory(authentication::keys());
                let id = Id::allocate();
                let value = envelope::seal(&inventory.encryption, account.person_id(), id, Code::Unaffiliated).expect("valid envelope");
                let now = context.sample().await.expect("trusted time");
                assert_eq!(runtime.client().execute("INSERT INTO center.affiliations (id,person_id,value,operator_id,created_at,operation_id) VALUES ($1,$2,$3,$4,$5,$6)", &[&id,&account.person_id(),&value,&account.revision_id(),&now,&operation()]).await.expect_err("sequence and seal are mandatory").code().map(tokio_postgres::error::SqlState::code), Some("23502"));
                assert_eq!(runtime.client().execute("INSERT INTO center.affiliations SELECT $1,person_id,value,operator_id,created_at+interval '1 second',operation_id,sequence+1,seal,seal_key_id,seal_version FROM center.affiliations WHERE operation_id=$2", &[&id,&original]).await.expect_err("permanent original-operation identity stays unique").code().map(tokio_postgres::error::SqlState::code), Some("23505"));
                assert_eq!(runtime.client().query_one("SELECT count(*) FROM center.affiliations", &[]).await.expect("rejected probes leave one record").get::<_,i64>(0), 1);
                runtime.terminate().await.expect("runtime ended");
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop"); supplier.stop().await;
            })).await.expect("structural SQL acceptance deadline");
        }));
    }

    #[test]
    fn sealed_state_record_and_completion_damage_is_rejected_during_use_and_startup() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in [
                "state_id", "state_pointer", "state_time", "state_sequence", "state_version",
                "state_seal", "state_key", "state_encoding", "state_rollback", "state_deleted",
                "record_gap", "record_latest", "record_sequence", "record_forged",
                "completion_family", "completion_operation", "completion_action", "completion_target",
                "completion_input", "completion_subject", "completion_person", "completion_revision",
                "completion_time", "completion_expiry", "completion_seal", "completion_key", "completion_encoding",
            ] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let administrator = actor(&database, &services, &mut supplier).await;
                    let member = register(&database, &services, &mut supplier).await;
                    let person = services.current().read(&member).await.expect("target person");
                    let original = operation();
                    services.affiliation().create(member.clone(), original, input("01")).await.expect("first state");
                    let owner = database.connections("postgres").recover().await.expect("isolated damage owner");
                    owner.client().batch_execute("CREATE TEMP TABLE original_profile AS TABLE center.profiles").await.expect("authentic earlier state");
                    for code in ["13", "01"] {
                        let correction = serde_json::from_value(json!({
                            "target": {"type":"account", "accountId":person.id(), "personId":person.person_id(), "revisionId":person.revision_id()},
                            "affiliation":code,
                        })).expect("confirmed target");
                        services.affiliation().correct(administrator.clone(), operation(), person.person_id(), correction).await.expect("another retained state");
                    }
                    let changed_operation = operation();
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("damage only this disposable database");
                    let statement = match case {
                        "state_id" => "UPDATE center.profiles SET id=gen_random_uuid()",
                        "state_pointer" => "UPDATE center.profiles SET affiliation_id=gen_random_uuid()",
                        "state_time" => "UPDATE center.profiles SET created_at=created_at-interval '1 second'",
                        "state_sequence" => "UPDATE center.profiles SET sequence=sequence+1",
                        "state_version" => "UPDATE center.profiles SET version=version+1",
                        "state_seal" => "UPDATE center.profiles SET seal=set_byte(seal,0,get_byte(seal,0) # 1)",
                        "state_key" => "UPDATE center.profiles SET seal_key_id=99",
                        "state_encoding" => "ALTER TABLE center.profiles DROP CONSTRAINT profiles_seal_version_check; UPDATE center.profiles SET seal_version=2",
                        "state_rollback" => "UPDATE center.profiles current SET affiliation_id=old.affiliation_id,created_at=old.created_at,sequence=old.sequence,version=old.version,seal=old.seal,seal_key_id=old.seal_key_id,seal_version=old.seal_version FROM original_profile old WHERE current.id=old.id",
                        "state_deleted" => "DELETE FROM center.profiles",
                        "record_gap" => "DELETE FROM center.affiliations WHERE sequence=2",
                        "record_latest" => "DELETE FROM center.affiliations WHERE sequence=3",
                        "record_sequence" => "UPDATE center.affiliations SET sequence=9 WHERE sequence=2",
                        "record_forged" => "INSERT INTO center.affiliations SELECT gen_random_uuid(),person_id,value,operator_id,created_at+interval '1 second',gen_random_uuid(),sequence+1,seal,seal_key_id,seal_version FROM center.affiliations WHERE sequence=3",
                        "completion_family" => "ALTER TABLE center.completions DROP CONSTRAINT completions_family_check; UPDATE center.completions SET family='workforce_configuration'",
                        "completion_operation" => "",
                        "completion_action" => "UPDATE center.completions SET action=CASE action WHEN 'fill' THEN 'correct' ELSE 'fill' END",
                        "completion_target" => "UPDATE center.completions SET target=gen_random_uuid()::text",
                        "completion_input" => "UPDATE center.completions SET input=set_byte(input,38,get_byte(input,38) # 1)",
                        "completion_subject" => "ALTER TABLE center.completions DROP CONSTRAINT completions_subject_check, DROP CONSTRAINT completions_subject_action_check; UPDATE center.completions SET subject='anonymous'",
                        "completion_person" => "",
                        "completion_revision" => "UPDATE center.completions SET revision_id=gen_random_uuid()",
                        "completion_time" => "UPDATE center.completions SET created_at=created_at-interval '1 second'",
                        "completion_expiry" => "ALTER TABLE center.completions DROP CONSTRAINT completions_expires_at_check; UPDATE center.completions SET expires_at=created_at+interval '24 hours'",
                        "completion_seal" => "UPDATE center.completions SET seal=set_byte(seal,0,get_byte(seal,0) # 1)",
                        "completion_key" => "UPDATE center.completions SET seal_key_id=99",
                        "completion_encoding" => "ALTER TABLE center.completions DROP CONSTRAINT completions_seal_version_check; UPDATE center.completions SET seal_version=2",
                        _ => unreachable!("closed damage cases"),
                    };
                    if case == "completion_operation" {
                        owner.client().execute("UPDATE center.completions SET operation_id=$2 WHERE operation_id=$1", &[&original,&changed_operation]).await.expect("move original completion");
                    } else if case == "completion_person" {
                        let other = services.current().read(&administrator).await.expect("another authenticated person");
                        owner.client().execute("UPDATE center.completions SET person_id=$2 WHERE operation_id=$1", &[&original,&other.person_id()]).await.expect("forge the completion subject");
                    } else {
                        owner.client().batch_execute(statement).await.expect("selected damage");
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage visible");
                    let snapshot = "SELECT jsonb_build_array((SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM center.profiles s),(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM center.affiliations r),(SELECT jsonb_agg(to_jsonb(c) ORDER BY operation_id) FROM center.completions c))::text";
                    let before: String = owner.client().query_one(snapshot, &[]).await.expect("damaged facts").get(0);
                    if matches!(case, "state_deleted" | "state_rollback") {
                        services.affiliation().create(member.clone(), original, input("01")).await.expect("known original replay never reads today's state");
                        services.ready().await.expect("an unread current-state fault has not been interpreted");
                    }
                    let error = if case.starts_with("completion_") {
                        services.affiliation().create(if case=="completion_person" { administrator.clone() } else { member.clone() }, if case=="completion_operation" { changed_operation } else { original }, input("01")).await.expect_err("completion damage prevents original replay")
                    } else {
                        services.affiliation().history(&member, Pagination::new(1,20).expect("complete window")).await.err().expect("state or history damage prevents a page")
                    };
                    assert!(matches!(error, Error::Corrupt(_)), "{case}: {error:?}");
                    assert!(services.ready().await.is_err(), "{case}: damaged data closes admission");
                    assert_eq!(owner.client().query_one(snapshot, &[]).await.expect("failed use does not repair facts").get::<_,String>(0), before);
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                    let result = Box::pin(crate::startup::Services::start(
                        crate::database::Barrier::begin(context.clone()).await.expect("restart barrier"),
                        authentication::inventory(authentication::keys()),
                        crate::startup::Settings::new(4,crate::identity::message::Limits::default(),authentication::session_settings(),crate::registration::Limits::default(),authentication::avatar_settings()).expect("settings"),
                        supplier.client(), authentication::unused_client().await,
                    )).await;
                    assert!(matches!(result, Err(crate::startup::Error::Affiliation(Error::Corrupt(_)))), "{case}: startup rejects damaged retained material");
                    context.stop().await.expect("failed startup stopped");
                    owner.terminate().await.expect("owner ended"); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("damage case {case}: {error}"));
            }
        }));
    }

    #[test]
    fn seals_reject_rewritten_facts_whose_envelopes_stay_valid() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let other = services.current().read(&administrator).await.expect("another real revision");
                let original = operation();
                services.affiliation().create(member.clone(), original, input("01")).await.expect("real first fill");
                let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                let inventory = authentication::inventory(authentication::keys());
                let keys = history::Keys { data: &inventory.encryption, seals: &inventory.seal };
                verify_history(owner.client(), keys, context.sample().await.expect("trusted time")).await.expect("the real fact authenticates");
                let saved: Vec<u8> = owner.client().query_one("SELECT seal FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("stored seal").get(0);
                // Each rewrite keeps the envelope valid, so only the seal can detect it.
                for (case, damage) in [
                    ("operator", "UPDATE center.affiliations SET operator_id=$2 WHERE operation_id=$1"),
                    ("time", "UPDATE center.affiliations SET created_at=created_at-interval '1 second' WHERE operation_id=$1"),
                    ("operation", "UPDATE center.affiliations SET operation_id=gen_random_uuid() WHERE operation_id=$1"),
                    ("seal", "UPDATE center.affiliations SET seal=set_byte(seal,0,get_byte(seal,0) # 1) WHERE operation_id=$1"),
                ] {
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                    let changed = if case == "operator" {
                        owner.client().execute(damage, &[&original, &other.revision_id()]).await
                    } else {
                        owner.client().execute(damage, &[&original]).await
                    };
                    assert_eq!(changed.expect("damage applied"), 1, "{case}");
                    let error = verify_history(owner.client(), keys, context.sample().await.expect("trusted time")).await.expect_err("rewritten fact is damage");
                    assert!(matches!(error, history::Error::Seal(crate::keys::seal::Error::Mismatch)), "{case}: {error:?}");
                    assert!(Error::from(error).is_corrupt());
                    owner.client().batch_execute("ROLLBACK").await.expect("restore the original fact");
                }
                let active = crate::keys::Id::new(1).expect("rotated key");
                let rotated = crate::keys::seal::Keys::new(crate::keys::Derived::fixed(active, vec![Entry::new(active, Material(Zeroizing::new([13; 32])))]));
                assert!(matches!(verify_history(owner.client(), history::Keys { data: &inventory.encryption, seals: &rotated }, context.sample().await.expect("trusted time")).await, Err(history::Error::Seal(crate::keys::seal::Error::Missing(_)))), "verification requires every retained seal key");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("committed damage");
                owner.client().execute("UPDATE center.affiliations SET created_at=created_at-interval '1 second' WHERE operation_id=$1", &[&original]).await.expect("earlier time");
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                assert!(services.affiliation().history(&member, crate::pagination::Pagination::new(1, 20).expect("page")).await.err().expect("history verifies seals").is_corrupt());
                assert!(services.ready().await.is_err(), "seal damage closes admission");
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                let restart = |keys: crate::keys::Keys| {
                    let database = &database;
                    let supplier = &supplier;
                    async move {
                        let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                        let result = Box::pin(crate::startup::Services::start(
                            crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                            keys,
                            crate::startup::Settings::new(4,crate::identity::message::Limits::default(),authentication::session_settings(),crate::registration::Limits::default(),authentication::avatar_settings()).expect("settings"),supplier.client(),authentication::unused_client().await,
                        )).await;
                        let result = match result {
                            Ok(services) => {
                                services.close();
                                services.drain().await.expect("drained");
                                Ok(())
                            }
                            Err(error) => Err(error),
                        };
                        context.stop().await.expect("stopped");
                        result
                    }
                };
                assert!(matches!(restart(authentication::inventory(authentication::keys())).await, Err(crate::startup::Error::Affiliation(Error::Corrupt(fault))) if matches!(fault.kind(), FaultKind::History(history::Error::Seal(crate::keys::seal::Error::Mismatch)))), "startup rejects a rewritten time");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("repair");
                owner.client().execute("UPDATE center.affiliations SET created_at=created_at+interval '1 second', seal=$2 WHERE operation_id=$1", &[&original, &saved]).await.expect("original time restored");
                owner.client().batch_execute("COMMIT").await.expect("repaired");
                // The CHECK keeps the version even in replica mode; drop it in this disposable database.
                owner.client().batch_execute("DO $$ DECLARE name text; BEGIN FOR name IN SELECT conname FROM pg_constraint WHERE conrelid = 'center.affiliations'::regclass AND contype = 'c' AND pg_get_constraintdef(oid) LIKE '%seal_version%' LOOP EXECUTE format('ALTER TABLE center.affiliations DROP CONSTRAINT %I', name); END LOOP; END $$; BEGIN; SET LOCAL session_replication_role=replica").await.expect("version damage");
                owner.client().execute("UPDATE center.affiliations SET seal_version=2 WHERE operation_id=$1", &[&original]).await.expect("other version");
                owner.client().batch_execute("COMMIT").await.expect("version committed");
                assert!(matches!(restart(authentication::inventory(authentication::keys())).await, Err(crate::startup::Error::Affiliation(Error::Corrupt(fault))) if matches!(fault.kind(), FaultKind::History(history::Error::Seal(crate::keys::seal::Error::Mismatch)))), "startup rejects an unknown seal version");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE center.affiliations SET seal_version=1; COMMIT").await.expect("version restored");
                restart(authentication::inventory(authentication::keys())).await.expect("the repaired fact authenticates again");
                owner.terminate().await.expect("owner ended"); supplier.stop().await;
            })).await.expect("bounded seal damage check");
        }));
    }

    #[test]
    fn personal_completion_replay_rejects_other_people_before_opening_input() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for damage in [
                "UPDATE center.completions SET input=set_byte(input,38,get_byte(input,38) # 1)",
                "UPDATE center.completions SET input=set_byte(input,8,99)",
                "UPDATE center.completions SET seal_key_id=99",
            ] {
                timeout(
                    Duration::from_secs(30),
                    Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let (context, services) =
                            start(database.connections(&database.user), &supplier).await;
                        let administrator = actor(&database, &services, &mut supplier).await;
                        let member = register(&database, &services, &mut supplier).await;
                        let original = operation();
                        services
                            .affiliation()
                            .create(member.clone(), original, input("01"))
                            .await
                            .expect("personal original");
                        let owner = database
                            .connections("postgres")
                            .recover()
                            .await
                            .expect("isolated damage owner");
                        owner
                            .client()
                            .batch_execute("BEGIN; SET LOCAL session_replication_role=replica")
                            .await
                            .expect("disposable database only");
                        owner
                            .client()
                            .batch_execute(damage)
                            .await
                            .expect("damaged completion");
                        owner
                            .client()
                            .batch_execute("COMMIT")
                            .await
                            .expect("damage visible");
                        for code in 1..=13 {
                            let result = services
                                .affiliation()
                                .create(
                                    administrator.clone(),
                                    original,
                                    input(&format!("{code:02}")),
                                )
                                .await;
                            assert!(
                                matches!(result, Err(Error::Mismatch)),
                                "another person cannot open the original input: {result:?}"
                            );
                        }
                        services.ready().await.expect(
                            "an unrelated person's replay did not interpret the damaged input",
                        );
                        assert!(
                            services
                                .affiliation()
                                .create(member, original, input("01"))
                                .await
                                .expect_err("the original person must authenticate the completion")
                                .is_corrupt()
                        );
                        assert!(
                            services.ready().await.is_err(),
                            "a matching person's replay detects corruption"
                        );
                        services.close();
                        services.drain().await.expect("drain");
                        context.stop().await.expect("stop");
                        owner.terminate().await.expect("owner ended");
                        supplier.stop().await;
                    }),
                )
                .await
                .expect("personal completion replay budget");
            }
        }));
    }

    #[test]
    fn retained_values_are_fully_verified_without_exposing_other_operators_values() {
        use platform::cryptography::envelope::{Binding, Input as EnvelopeInput, Key};

        struct Raw {
            identity: [u8; 32],
            plaintext: [u8; 2],
        }
        impl Binding for Raw {
            fn domain(&self) -> &[u8] {
                b"nucleus/center-affiliation/v1"
            }
            fn identity(&self) -> &[u8] {
                &self.identity
            }
            fn field(&self) -> &[u8] {
                b"affiliation.value"
            }
        }
        impl EnvelopeInput for Raw {
            fn plaintext(&self) -> &[u8] {
                &self.plaintext
            }
        }
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35),Box::pin(async {
                let database=Database::initialized(server).await;let mut supplier=Supplier::new().await;
                let(context,services)=start(database.connections(&database.user),&supplier).await;
                let administrator=actor(&database,&services,&mut supplier).await;
                let member=register(&database,&services,&mut supplier).await;
                let account=services.current().read(&member).await.expect("person");
                let admin=services.current().read(&administrator).await.expect("other person");
                let original=operation(); services.affiliation().create(member.clone(),original,input("01")).await.expect("permanent first value");
                let owner=database.connections("postgres").recover().await.expect("isolated corruption owner");
                let row=owner.client().query_one("SELECT id,value,created_at FROM center.affiliations WHERE operation_id=$1",&[&original]).await.expect("original fact");
                let id:Id=row.get("id");let created_at:DateTime<Utc>=row.get("created_at");
                let inventory=authentication::inventory(authentication::keys());
                let keys=history::Keys { data: &inventory.encryption, seals: &inventory.seal };
                let records = (1..=128_u128).map(|number| affiliation::database::Data {
                    id: Id(Uuid::from_u128(number)), code: Code::Unaffiliated,
                    operator_id: account.revision_id(), operator_person: account.person_id(),
                    created_at: created_at-chrono::Duration::microseconds(i64::try_from(number).expect("small fixture count")),
                    operation_id: operation(),
                }).collect();
                seed_history(owner.client(), services.affiliation(), account.person_id(), records).await;
                let original_value: Vec<u8> = owner.client().query_one("SELECT value FROM center.affiliations WHERE operation_id=$1", &[&original]).await.expect("seeded original envelope").get(0);
                verify_history(owner.client(),keys,context.sample().await.expect("trusted time")).await.expect("all 129 retained rows authenticate");
                let mut identity=[0;32];identity[..16].copy_from_slice(account.person_id().as_bytes());identity[16..].copy_from_slice(id.as_bytes());
                let raw_key=Key::new(crate::keys::Id::new(0).expect("fixture key"),Zeroizing::new([9;32]));
                for case in ["tag","version","key","person","row","code","utf8","time","operator"] {
                    let mut value=original_value.clone();
                    match case {
                        "tag"=>value[38]^=1,
                        "version"=>value[0]=2,
                        "key"=>value[8]=7,
                        "code"|"utf8"=>value=raw_key.seal(&Raw{identity,plaintext:if case=="code"{*b"14"}else{[255,255]}}).expect("authentic but invalid plaintext").as_ref().to_vec(),
                        _=>(),
                    }
                    let changed_id=if case=="row"{Id::allocate()}else{id};
                    let person_id=if case=="person"{admin.person_id()}else{account.person_id()};
                    let operator_id=match case {
                        "operator"=>person::revision::Id::allocate(),
                        _=>account.revision_id(),
                    };
                    let at=if case=="time"{created_at+chrono::Duration::days(1)}else{created_at};
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("privileged corruption only in disposable DB");
                    owner.client().execute("UPDATE center.affiliations SET id=$2,person_id=$3,operator_id=$4,created_at=$5,value=$6 WHERE operation_id=$1",&[&original,&changed_id,&person_id,&operator_id,&at,&value]).await.expect("controlled fault");
                    owner.client().batch_execute("COMMIT").await.expect("fault committed");
                    let error=verify_history(owner.client(),keys,context.sample().await.expect("trusted time")).await.expect_err("damaged retained fact cannot mean absent");
                    let error = Error::from(error);
                    assert!(error.is_corrupt(), "{case}: {error:?}");
                }
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("restore valid identity and time");
                let mut corrupt=original_value.clone();corrupt[38]^=1;
                owner.client().execute("UPDATE center.affiliations SET id=$2,person_id=$3,operator_id=$4,created_at=$5,value=$6 WHERE operation_id=$1",&[&original,&id,&account.person_id(),&account.revision_id(),&created_at,&corrupt]).await.expect("only bad tag remains");
                owner.client().batch_execute("COMMIT").await.expect("corrupt fact");
                for code in 1..=13 {
                    assert!(matches!(services.affiliation().create(administrator.clone(),original,input(&format!("{code:02}"))).await,Err(Error::Mismatch)),"identity precedes decryption");
                }
                services.ready().await.expect("another person's request did not interpret the value");
                assert!(services.affiliation().create(member,original,input("01")).await.expect_err("own original authenticates the value").is_corrupt());
                assert!(services.ready().await.is_err(),"unreliable data closes admission");
                services.close();services.drain().await.expect("drain");context.stop().await.expect("stop");
                for case in ["ciphertext", "operator"] {
                    if case == "operator" {
                        owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated operator corruption");
                        owner.client().execute("UPDATE center.affiliations SET operator_id=$2,value=$3 WHERE operation_id=$1", &[&original,&person::revision::Id::allocate(),&original_value]).await.expect("authentic value with a missing operator");
                        owner.client().batch_execute("COMMIT").await.expect("operator fault committed");
                    }
                    let context=crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                    let result=Box::pin(crate::startup::Services::start(
                        crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                        authentication::inventory(authentication::keys()),
                        crate::startup::Settings::new(4,crate::identity::message::Limits::default(),authentication::session_settings(),crate::registration::Limits::default(),authentication::avatar_settings()).expect("settings"),supplier.client(),authentication::unused_client().await,
                    )).await;
                    if case == "operator" {
                        assert!(matches!(result,Err(crate::startup::Error::Affiliation(Error::Corrupt(fault))) if matches!(fault.kind(), FaultKind::History(history::Error::Seal(crate::keys::seal::Error::Mismatch)))),"startup rejects a rewritten operator");
                    } else {
                        assert!(matches!(result,Err(crate::startup::Error::Affiliation(Error::Corrupt(fault))) if matches!(fault.kind(), FaultKind::History(history::Error::Encryption(_)))),"startup rejects damaged ciphertext");
                    }
                    context.stop().await.expect("failed startup context stopped");
                }
                owner.terminate().await.expect("owner ended");supplier.stop().await;
            })).await.expect("retained value verification deadline");
        }));
    }
}
