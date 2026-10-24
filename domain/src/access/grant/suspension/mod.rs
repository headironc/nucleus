//! Suspend a current Grant while preserving its membership and account.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::timeout_at;
use tokio_postgres::{
    Client,
    types::{FromSql, ToSql},
};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    access::{
        grant::{
            self, CommandError, Error, Service,
            lifecycle::{self, Kind},
            member,
        },
        role,
    },
    history,
    identity::{
        current::{self, Actor},
        mobile::Mobile,
        operation,
        person::revision,
    },
    kernel::{Authorization, Command, completion, operation::Family, tx::Tx},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = grant::suspension::Id, value_type = Uuid)]
pub struct Id(Uuid);

impl Id {
    fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(super) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = grant::suspension::Input)]
pub struct Input {
    target: member::Target,
}

pub struct Suspension {
    id: Id,
    grant_id: grant::Id,
    subject: revision::Identity,
    operator: revision::Identity,
    suspended_at: DateTime<Utc>,
    sequence: i64,
    operation_id: operation::Id,
}

impl Suspension {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn grant_id(&self) -> grant::Id {
        self.grant_id
    }
    pub fn subject(&self) -> &revision::Identity {
        &self.subject
    }
    pub fn operator(&self) -> &revision::Identity {
        &self.operator
    }
    pub fn suspended_at(&self) -> DateTime<Utc> {
        self.suspended_at
    }
    pub fn sequence(&self) -> i64 {
        self.sequence
    }
    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
}

pub struct List {
    count: i64,
    suspensions: Vec<Suspension>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn suspensions(&self) -> &[Suspension] {
        &self.suspensions
    }
}

impl Suspension {
    fn from_fact(fact: lifecycle::Fact) -> Result<Self, Error> {
        let Kind::Suspension(id) = fact.kind else {
            return Err(Error::Facts {
                rule: "suspension delivery retains its exact record kind",
            });
        };
        Ok(Self {
            id,
            grant_id: fact.grant_id,
            subject: fact.subject,
            operator: fact.operator,
            suspended_at: fact.happened_at,
            sequence: fact.sequence,
            operation_id: fact.operation_id,
        })
    }
}

pub(super) async fn find_open_by_grant(
    client: &Client,
    roles: &role::Reader,
    grant_id: grant::Id,
    now: DateTime<Utc>,
) -> Result<Option<Id>, Error> {
    Ok(
        grant::state::find(client, roles, grant_id, roles.read()?, now)
            .await?
            .and_then(|state| state.suspension_id()),
    )
}

impl Service {
    pub async fn list_suspensions(
        &self,
        actor: &Actor,
        filter: grant::Filter,
    ) -> Result<List, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let filter = &filter;
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    async move {
                        let (_, _, _, now) = member::authorize(
                            client,
                            &roles,
                            &self.current,
                            actor,
                            role::Code::WorkforceSuspension,
                        )
                        .await?;
                        self.fetch_suspensions(client, &roles, filter, now).await
                    }
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    async fn fetch_suspensions(
        &self,
        client: &Client,
        roles: &role::Reader,
        filter: &grant::Filter,
        now: DateTime<Utc>,
    ) -> Result<List, Error> {
        let (count, facts) =
            lifecycle::page(self, client, roles, filter, "suspension", now).await?;
        let suspensions = facts
            .into_iter()
            .map(Suspension::from_fact)
            .collect::<Result<_, _>>()?;
        Ok(List { count, suspensions })
    }
    pub async fn suspension_candidate(
        &self,
        actor: &Actor,
        mobile: Mobile,
    ) -> Result<member::Candidate, Error> {
        self.member_candidate(actor, mobile, role::Code::WorkforceSuspension)
            .await
    }

    pub async fn suspend(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        grant_id: grant::Id,
        input: Input,
    ) -> Result<Suspension, Error> {
        Suspension::from_fact(
            self.app
                .command(
                    operation_id,
                    Lifecycle(lifecycle::Command {
                        service: self.clone(),
                        actor,
                        action: lifecycle::Action::Suspend {
                            grant_id,
                            id: Id::allocate(),
                            target: input.target,
                        },
                    }),
                )
                .await?,
        )
    }
}

fn accepts(action: &str, target: &str) -> bool {
    action == "suspend" && Uuid::parse_str(target).is_ok_and(|id| id.to_string() == target)
}
pub(super) const COMPLETIONS: completion::Template =
    completion::Template::personal(Family::GrantSuspension, accepts);

struct Lifecycle(lifecycle::Command);
impl Command for Lifecycle {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    type Facts = lifecycle::Facts;
    type Change = lifecycle::Selected;
    type Output = lifecycle::Fact;
    type Rejection = grant::Rejection;
    fn session(&self) -> Option<(&current::Service, &Actor)> {
        self.0.session()
    }
    fn request(&self, authority: &Authorization) -> Result<completion::Request, CommandError> {
        self.0.request(authority)
    }
    async fn locks(&self, tx: &Tx) -> Result<(), CommandError> {
        self.0.locks(tx).await
    }
    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, CommandError> {
        self.0.authorize(tx, subject).await
    }
    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Self::Facts, CommandError> {
        self.0.load(tx, authority).await
    }
    fn decide(&self, facts: Self::Facts, now: DateTime<Utc>) -> Result<Self::Change, CommandError> {
        self.0.decide(facts, now)
    }
    async fn apply(
        &self,
        tx: &Tx,
        selected: Self::Change,
        pending: &mut Option<history::Pending>,
    ) -> Result<Self::Output, CommandError> {
        self.0.apply(tx, selected, pending).await
    }
    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Self::Output, CommandError> {
        self.0.replay(tx, request, now).await
    }
}

#[cfg(test)]
pub(super) mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::{
            appointment::tests::register,
            grant::{self, Candidate, Entry, Error, Status, member},
            role::tests::{actor, start},
        },
        identity::{
            authentication::{Database, Supplier, server},
            current, operation,
        },
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    pub(in crate::access::grant) fn admission(
        candidate: &Candidate,
        entry: &str,
        mobile: &str,
        index: usize,
    ) -> grant::Input {
        let subject = candidate.identity();
        let kind = if index == 0 {
            crate::access::role::Kind::Default
        } else {
            crate::access::role::Kind::Full
        };
        let role = candidate
            .roles()
            .iter()
            .find(|role| role.kind() == kind)
            .expect("selected system role");
        serde_json::from_value(json!({"entry":entry,"target":{
            "accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),
            "mobile":mobile,"assignmentId":candidate.assignment_id(),
            "suspensionId":candidate.suspension_id(),"grant":candidate.grant()
        },"role":{"id":role.id(),"changeId":role.change_id()}})).expect("typed admission")
    }

    pub(in crate::access::grant) fn confirmation(
        candidate: &member::Candidate,
        mobile: &str,
    ) -> serde_json::Value {
        let subject = candidate.identity();
        json!({"target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":mobile}})
    }

    pub(in crate::access::grant) async fn participants(
        database: &Database,
        services: &crate::startup::Services,
        supplier: &mut Supplier,
    ) -> (current::Actor, current::Actor, member::Candidate) {
        let holder = Box::pin(actor(database, services, supplier)).await;
        let manager = Box::pin(register(database, services, supplier)).await;
        let candidate = services
            .grant()
            .candidate(
                &holder,
                Entry::Institution,
                "13800000000".parse().expect("mobile"),
                crate::pagination::Pagination::new(1, 20).expect("pagination"),
            )
            .await
            .expect("domain manager candidate");
        Box::pin(services.grant().create(
            holder.clone(),
            operation(),
            admission(&candidate, "institution", "13800000000", 1),
        ))
        .await
        .expect("institution admits domain administrator");
        let candidate = services
            .grant()
            .candidate(
                &manager,
                Entry::Domain,
                "13900000000".parse().expect("mobile"),
                crate::pagination::Pagination::new(1, 20).expect("pagination"),
            )
            .await
            .expect("higher institution rank is irrelevant");
        Box::pin(services.grant().create(
            manager.clone(),
            operation(),
            admission(&candidate, "domain", "13900000000", 0),
        ))
        .await
        .expect("ordinary member");
        let candidate = services
            .grant()
            .suspension_candidate(&manager, "13900000000".parse().expect("mobile"))
            .await
            .expect("managed member");
        (manager, holder, candidate)
    }

    #[test]
    fn suspension_sql_pairs_grant_state_and_records_and_allows_suspended_account_targets() {
        use tokio_postgres::error::SqlState;

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (manager, institution, _) = Box::pin(participants(&database, &services, &mut supplier)).await;
                let message_id = authentication::session::tests::message(&database, services.authentication(), &mut supplier, "13700000000").await;
                let authentication::session::Receipt::Registration(credential) = Box::pin(services.authentication().create_session(authentication::session::tests::input(Uuid::new_v4(), message_id, "13700000000", authentication::session::tests::CODE))).await.expect("peer R1") else { panic!("registration credential"); };
                let registered = Box::pin(services.registration().register(crate::registration::Input::new(operation(), credential.id(), credential.token().text().parse().expect("proof"), "Other Member".parse().expect("name"), "000000200002290088".parse().expect("id number")))).await.expect("peer R2");
                let peer = services.current().authenticate(current::Proof { id: registered.id(), token: registered.token().text().parse().expect("token") }).await.expect("peer actor");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13700000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("peer confirmation");
                let (peer_grant, _) = Box::pin(services.grant().create(institution.clone(), operation(), admission(&candidate, "institution", "13700000000", 1))).await.expect("peer administrator");
                assert!(matches!(services.grant().suspension_candidate(&manager, "13700000000".parse().expect("mobile")).await, Err(Error::Absent)));
                let mut stale = json!({"target":{"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":Uuid::new_v4(),"mobile":"13700000000"}});
                assert!(matches!(Box::pin(services.grant().suspend(manager.clone(), operation(), peer_grant.id(), serde_json::from_value(stale.clone()).expect("stale peer input"))).await, Err(Error::Absent)), "peer remains concealed before confirmation comparison");
                let operator = services.current().read(&manager).await.expect("operator revision");
                let writer = database.connections(&database.user).recover().await.expect("runtime writer");
                grant::tests::sql_transition(&database, &services, peer_grant.id(), operator.revision_id(), grant::tests::SqlAction::Suspend).await;
                Box::pin(services.grant().terminate(peer.clone(), operation(), peer_grant.id())).await.expect("peer exits");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13700000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("ordinary rejoin");
                let (grant, _) = Box::pin(services.grant().create(institution.clone(), operation(), admission(&candidate, "institution", "13700000000", 0))).await.expect("ordinary new grant");
                let candidate = services.grant().suspension_candidate(&manager, "13700000000".parse().expect("mobile")).await.expect("manageable target");
                grant::tests::sql_transition(&database, &services, grant.id(), operator.revision_id(), grant::tests::SqlAction::Suspend).await;
                let pause = Box::pin(services.suspension().suspend(institution.clone(), operation(), candidate.identity().id(), serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13700000000"})).expect("account confirmation"))).await.expect("account suspension retains domain membership");
                services.grant().suspension_candidate(&manager, "13700000000".parse().expect("mobile")).await.expect("suspended account remains manageable");
                let original = operation();
                let body = confirmation(&candidate,"13700000000");
                let suspended = Box::pin(services.grant().suspend(manager.clone(), original, grant.id(), serde_json::from_value(body.clone()).expect("confirmed target"))).await.expect("domain suspension of a suspended account");
                assert_eq!(suspended.sequence(),2);
                assert!(writer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.suspensions WHERE id=$1 AND sequence=2) AND NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE id=$1) AND EXISTS(SELECT 1 FROM access.grants WHERE id=$2 AND termination IS NULL) AND EXISTS(SELECT 1 FROM access.suspensions WHERE id=$3 AND sequence=2) AND EXISTS(SELECT 1 FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id WHERE account.suspension_id=$1 AND account.sequence=2 AND person.sequence=1)", &[&pause.id(),&grant.id(),&suspended.id()]).await.expect("independent Account and Person sequences retain both suspensions and membership").get::<_,bool>(0));
                for sql in ["UPDATE access.suspensions SET suspended_at=suspended_at", "DELETE FROM access.suspensions"] {
                    assert_eq!(writer.client().batch_execute(sql).await.expect_err("runtime cannot rewrite history").code(),Some(&SqlState::INSUFFICIENT_PRIVILEGE));
                }
                assert!(matches!(Box::pin(services.grant().terminate(peer,operation(),grant.id())).await,Err(Error::Current(current::Error::Invalid))),"account suspension still prevents self exit");
                stale["target"]["revisionId"] = json!(candidate.identity().revision_id());
                Box::pin(services.grant().terminate_member(manager.clone(),operation(),grant.id(),serde_json::from_value(stale).expect("termination confirmation"))).await.expect("manager ends a suspended grant and account");
                Box::pin(services.cancellation().cancel_account(institution,operation(),candidate.identity().id(),serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13700000000"})).expect("cancellation"))).await.expect("later cancellation preserves domain history");
                let replay = Box::pin(services.grant().suspend(manager,original,grant.id(),serde_json::from_value(body).expect("original input"))).await.expect("original suspension survives target cancellation");
                assert_eq!(replay.id(),suspended.id());
                assert!(writer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.suspensions WHERE id=$1 AND sequence=2) AND EXISTS(SELECT 1 FROM access.terminations WHERE id=$2 AND sequence=3) AND EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$3 AND sequence=2) AND NOT EXISTS(SELECT 1 FROM access.grants WHERE id=$2 AND termination IS NULL)",&[&suspended.id(),&grant.id(),&candidate.identity().person_id()]).await.expect("independent permanent chronology").get::<_,bool>(0));
                writer.terminate().await.expect("writer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.close();services.drain().await.expect("all retained states validate on restart");context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("suspension database boundary deadline");
        }));
    }

    pub(in crate::access::grant) async fn wait_for_lock(
        client: &tokio_postgres::Client,
        blocker: i32,
        query: &str,
    ) {
        timeout(Duration::from_secs(3), async { loop {
            if client.query_one("WITH RECURSIVE blocked(pid) AS (SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) UNION SELECT backend.pid FROM pg_stat_activity backend JOIN blocked ON blocked.pid=ANY(pg_blocking_pids(backend.pid)) WHERE backend.datname=current_database()) SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid IN (SELECT pid FROM blocked) AND wait_event_type='Lock' AND query LIKE $2)", &[&blocker,&query]).await.expect("actual wait graph").get::<_,bool>(0) { break; }
            tokio::task::yield_now().await;
        }}).await.expect("operation reached its participant lock");
    }

    #[test]
    fn suspension_rechecks_participation_after_waiting_and_serializes_competing_pauses() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["manager exits", "holder exits", "duplicate", "different operation"] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (manager, holder, candidate) = Box::pin(participants(&database, &services, &mut supplier)).await;
                    let id = candidate.grant_id();
                    let target = if case == "manager exits" { services.grant().read_latest(&manager).await.expect("manager grant").id() } else { id };
                    let blocker = database.connections("postgres").recover().await.expect("isolated grant blocker");
                    blocker.client().batch_execute("BEGIN").await.expect("block participant");
                    blocker.client().query_one("SELECT id FROM access.grants WHERE id=$1 FOR UPDATE", &[&target]).await.expect("exclusive grant lock");
                    let pid: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("blocker PID").get(0);
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let service = services.grant().clone(); let actor = if case == "manager exits" { manager.clone() } else { holder.clone() };
                    let first = operation(); let input = serde_json::from_value(confirmation(&candidate,"13900000000")).expect("first confirmation"); let operator = manager.clone();
                    let preceding = tokio::spawn(async move {
                        if case.ends_with("exits") { service.terminate(actor, first, target).await.map(|_| ()) }
                        else { service.suspend(operator, first, id, input).await.map(|_| ()) }
                    });
                    wait_for_lock(observer.client(), pid, "SELECT id FROM access.grants WHERE id=$1 FOR UPDATE").await;
                    let service = services.grant().clone(); let input = serde_json::from_value(confirmation(&candidate,"13900000000")).expect("competing confirmation");
                    let operation_id = if case == "duplicate" { first } else { operation() };
                    let mut pending = tokio::spawn(async move { service.suspend(manager, operation_id, id, input).await });
                    if case == "duplicate" {
                        assert!(timeout(Duration::from_millis(50), &mut pending).await.is_err(), "same-family duplicate waits for the original work");
                    } else {
                        wait_for_lock(observer.client(), pid, if case == "manager exits" { "SELECT id FROM access.grants WHERE id=$1 FOR SHARE" } else { "SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE" }).await;
                    }
                    blocker.client().batch_execute("COMMIT").await.expect("release participant");
                    timeout(Duration::from_secs(5), preceding).await.expect("first operation deadline").expect("first task").expect("first accepted action");
                    let result = timeout(Duration::from_secs(5), pending).await.expect("second operation deadline").expect("second task");
                    match case {
                        "manager exits" => assert!(matches!(result, Err(Error::Denied))),
                        "holder exits" => assert!(matches!(result, Err(Error::Absent))),
                        "duplicate" => assert_eq!(result.expect("same original result").operation_id(),first),
                        _ => assert!(matches!(result, Err(Error::StateConflict))),
                    }
                    let paused = i64::from(!case.ends_with("exits"));
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.suspensions),(SELECT count(*) FROM access.terminations),(SELECT count(*) FROM access.grants WHERE termination IS NULL)",&[]).await.expect("only the first Grant transition commits");
                    assert_eq!(row.get::<_,i64>(0),paused);assert_eq!(row.get::<_,i64>(1),1-paused);assert_eq!(row.get::<_,i64>(2),1+paused);
                    services.current().read(&holder).await.expect("domain actions preserve holder session");
                    observer.terminate().await.expect("observer ended");blocker.terminate().await.expect("blocker ended");
                    services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("suspension lock coordination deadline");
            }
        }));
    }

    #[test]
    fn suspension_survives_direct_cancellation_and_rejects_damaged_old_history() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["cancellation", "missing", "future", "sequence"] {
                timeout(Duration::from_secs(40), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (manager, holder, candidate) = Box::pin(participants(&database, &services, &mut supplier)).await;
                    let original = operation(); let body = confirmation(&candidate,"13900000000"); let id = candidate.grant_id();
                    let paused = Box::pin(services.grant().suspend(manager.clone(),original,id,serde_json::from_value(body.clone()).expect("confirmation"))).await.expect("active member suspended");
                    if case=="cancellation" {
                        Box::pin(services.cancellation().cancel(holder,operation(),serde_json::from_value(json!({"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id()})).expect("self cancellation confirmation"))).await.expect("normal account may cancel during domain suspension");
                        let observer = database.connections(&database.user).recover().await.expect("independent observer");
                        assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.suspensions WHERE id=$1 AND sequence=2) AND EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$2 AND sequence=2) AND EXISTS(SELECT 1 FROM access.terminations ended JOIN access.grants state ON state.id=ended.id WHERE ended.id=$3 AND ended.mode='cancellation' AND ended.sequence=3 AND state.sequence=3 AND state.termination='cancellation' AND ended.cancellation_id=(SELECT id FROM identity.cancellations WHERE person_id=$2)) AND NOT EXISTS(SELECT 1 FROM access.grants WHERE id=$3 AND termination IS NULL)",&[&paused.id(),&candidate.identity().person_id(),&id]).await.expect("cancellation retains its own source and advances the Grant independently").get::<_,bool>(0));
                        services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                        let (context,services)=start(database.connections(&database.user),&supplier).await;
                        assert_eq!(Box::pin(services.grant().suspend(manager,original,id,serde_json::from_value(body).expect("original input"))).await.expect("original suspension after cancellation and restart").id(),paused.id());
                        observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                    } else {
                        Box::pin(services.grant().terminate(holder.clone(),operation(),id)).await.expect("self exit");
                        let candidate = services.grant().candidate(&manager,Entry::Domain,"13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("new candidate");
                        let (replacement,_)=Box::pin(services.grant().create(manager.clone(),operation(),admission(&candidate,"domain","13900000000",0))).await.expect("active new grant");
                        let owner=database.connections("postgres").recover().await.expect("isolated corruption owner");
                        owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated historical fault");
                        let sql=match case { "missing"=>"DELETE FROM access.suspensions WHERE id=$1", "future"=>"UPDATE access.suspensions SET suspended_at=clock_timestamp()+interval '1 day' WHERE id=$1", "sequence"=>"UPDATE access.suspensions SET sequence=sequence+1000 WHERE id=$1", _=>unreachable!() };
                        assert_eq!(owner.client().execute(sql,&[&paused.id()]).await.expect("damage only retained old suspension"),1);
                        owner.client().batch_execute("COMMIT").await.expect("fault installed");
                        let sequence = crate::identity::person::verify(owner.client(), services.app().keys(), chrono::Utc::now()).await;
                        let access = services.grant().verify(owner.client()).await;
                        assert!(sequence.is_err() || access.is_err(),"startup's identity and access owners reject {case}");
                        Box::pin(services.grant().terminate(holder,operation(),replacement.id())).await.expect("new Grant does not consume the old suspension");
                        services.ready().await.expect("unread old damage does not close admission");
                        let error=Box::pin(services.grant().suspend(manager,original,id,serde_json::from_value(body).expect("original input"))).await.err().expect("replay authenticates its original suspension");
                        assert_eq!(error.failure(),grant::Failure::Unavailable,"{case}: {error:?}");
                        assert!(services.ready().await.is_err(),"history damage closes acceptance");
                        assert!(owner.client().query_one("SELECT EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND termination='self' AND sequence=2) AND EXISTS(SELECT 1 FROM access.terminations WHERE id=$1 AND sequence=2) AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$2 AND sequence=1)",&[&replacement.id(),&candidate.identity().person_id()]).await.expect("new Grant ended without advancing personal order or repairing old history").get::<_,bool>(0));
                        owner.terminate().await.expect("owner ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                    }
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("retained suspension history deadline");
            }
        }));
    }

    #[test]
    fn suspension_replay_retains_its_record_and_state_reads_reject_missing_grants() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for missing in ["grant", "operator"] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (manager, holder, candidate) = Box::pin(participants(&database, &services, &mut supplier)).await;
                    let operation_id = operation();
                    let body = confirmation(&candidate, "13900000000");
                    let input = serde_json::from_value(body).expect("original confirmation");
                    let paused = Box::pin(services.grant().suspend(manager.clone(), operation_id, candidate.grant_id(), input)).await.expect("committed suspension");
                    let input = serde_json::from_value(confirmation(&candidate, "13900000000")).expect("same original confirmation");
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated missing reference");
                    let removed = if missing == "grant" {
                        owner.client().execute("DELETE FROM access.grants WHERE id=$1", &[&candidate.grant_id()]).await
                    } else {
                        owner.client().execute("DELETE FROM identity.revisions WHERE id=$1", &[&paused.operator().revision_id()]).await
                    }.expect("remove the exact referenced fact");
                    assert_eq!(removed, 1);
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    assert!(owner.client().query_one("SELECT EXISTS(SELECT 1 FROM access.suspensions WHERE id=$1 AND operation_id=$2)", &[&paused.id(), &operation_id]).await.expect("original suspension still exists").get::<_, bool>(0));

                    let verified = services.grant().verify(owner.client()).await;
                    assert!(verified.is_err(), "startup rejects the same broken reference");
                    let replay = Box::pin(services.grant().suspend(manager, operation_id, candidate.grant_id(), input)).await;
                    let error = if missing=="grant" {
                        let replay = replay.expect("the fixed suspension result does not read current Grant state");
                        assert_eq!(replay.id(), paused.id());
                        assert_eq!(replay.suspended_at(), paused.suspended_at());
                        services.grant().read_latest(&holder).await.err().expect("taking the missing state rejects its anchored absence")
                    } else {
                        replay.err().expect("the original result retains its fixed operator identity")
                    };
                    assert!(crate::runtime::work::Failure::unreliable(&error), "{missing}: {error:?}");
                    assert_eq!(error.failure(), grant::Failure::Unavailable);
                    assert!(services.ready().await.is_err(), "unreliable facts close acceptance");
                    let retained_grants = if missing=="grant" {1_i64} else {2_i64};
                    assert!(owner.client().query_one("SELECT (SELECT count(*) FROM access.suspensions)=1 AND (SELECT count(*) FROM access.grants)=$2 AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$1 AND sequence=1)", &[&candidate.identity().person_id(), &retained_grants]).await.expect("replay and failed reads recreate no state or history").get::<_, bool>(0));
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("missing suspension reference deadline");
            }
        }));
    }

    #[test]
    fn suspension_keeps_its_own_permanent_input_key_reference() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (manager, holder, candidate) = Box::pin(participants(&database, &services, &mut supplier)).await;
                let operation_id = operation(); let body = confirmation(&candidate,"13900000000");
                Box::pin(services.grant().suspend(manager.clone(), operation_id, candidate.grant_id(), serde_json::from_value(body.clone()).expect("confirmation"))).await.expect("suspension");
                let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; INSERT INTO security.keys(id,fingerprint,active) SELECT 1,fingerprint,false FROM security.keys WHERE id=0; UPDATE access.completions SET seal_key_id=1 WHERE family='grant_suspension'; COMMIT").await.expect("only suspension completion refers to unavailable old material");
                assert!(owner.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM access.grants WHERE seal_key_id<>0) AND EXISTS(SELECT 1 FROM access.completions WHERE family='grant_suspension' AND seal_key_id=1)",&[]).await.expect("distinct retained input reference").get::<_,bool>(0));
                assert!(services.grant().verify(owner.client()).await.is_err(), "startup must include suspension proof references");
                let error = Box::pin(services.grant().suspend(manager, operation_id, candidate.grant_id(), serde_json::from_value(body).expect("original confirmation"))).await.err().expect("unavailable material is not mismatch or success");
                assert_eq!(error.failure(),grant::Failure::Unavailable);
                assert!(services.ready().await.is_err(), "retained proof failure closes acceptance");
                assert!(owner.client().query_one("SELECT (SELECT count(*) FROM access.suspensions)=1 AND EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND termination IS NULL) AND EXISTS(SELECT 1 FROM identity.accounts WHERE id=$2)",&[&candidate.grant_id(),&holder.account_id()]).await.expect("failure preserves original facts and membership").get::<_,bool>(0));
                owner.terminate().await.expect("owner ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("retained suspension key deadline");
        }));
    }

    #[test]
    fn suspension_recovers_proven_input_and_retains_accepted_work() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["lost", "rollback", "unknown", "cleanup", "cancelled", "revoked"] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let (manager, holder, candidate) = Box::pin(participants(&database, &services, &mut supplier)).await;
                    let body = confirmation(&candidate, "13900000000");
                    let id = candidate.grant_id();
                    let operation_id = operation();
                    if case == "revoked" {
                        let original = crate::recovery::after_revocation(&database, &proxy, services.revocation(), &manager,
                            Box::pin(services.grant().suspend(manager.clone(), operation_id, id, serde_json::from_value(body.clone()).expect("confirmation"))))
                            .await.expect("later revocation cannot discard accepted work");
                        assert_eq!(original.grant_id(), id);
                        assert!(matches!(Box::pin(services.grant().suspend(manager.clone(), operation_id, id, serde_json::from_value(body.clone()).expect("confirmation"))).await, Err(Error::Current(current::Error::Invalid))));
                        services.close(); services.drain().await.expect("drained");
                    } else {
                        proxy.arm(match case { "lost" => Fault::LostCommit, "rollback" => Fault::RolledBack, "unknown" => Fault::UnknownCommit, "cleanup" | "cancelled" => Fault::DelayedCommit, _ => unreachable!() });
                        let service = services.grant().clone(); let actor = manager.clone(); let input = serde_json::from_value(body.clone()).expect("confirmation");
                        let waiter = tokio::spawn(async move { service.suspend(actor, operation_id, id, input).await });
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("actual original commit intercepted");
                        if case == "cleanup" { proxy.arm(Fault::ReadCommitTermination); }
                        if case == "cancelled" {
                            waiter.abort(); assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                            services.close();
                            assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "cancelled wait retains accepted work");
                            proxy.release(); services.drain().await.expect("accepted work drained");
                        } else {
                            proxy.release();
                            let result = timeout(Duration::from_secs(15), waiter).await.expect("bounded outcome").expect("waiter");
                            match case { "rollback" => assert!(matches!(result, Err(Error::Command(crate::kernel::Error::Unavailable(ref fault))) if fault.stage()==crate::kernel::Stage::Commit), "{:?}", result.as_ref().err()), "unknown" => assert!(matches!(result, Err(Error::Command(crate::kernel::Error::Unknown(_))))), _ => assert_eq!(result.expect("permanent suspension").sequence(), 2) }
                            services.close(); services.drain().await.expect("drained");
                        }
                    }
                    context.stop().await.expect("stopped");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT id FROM access.grants WHERE id=$1 FOR UPDATE", &[&id]).await.expect("original backend ended");
                    let committed = i64::from(case != "rollback");
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.grants),(SELECT count(*) FROM access.assignments),(SELECT count(*) FROM access.suspensions),(SELECT count(*) FROM access.grants WHERE termination IS NULL),sequence FROM access.grants WHERE id=$1", &[&id]).await.expect("independent durable state before replay");
                    assert_eq!(row.get::<_,i64>(0),2); assert_eq!(row.get::<_,i64>(1),2); assert_eq!(row.get::<_,i64>(2),committed); assert_eq!(row.get::<_,i64>(3),2); assert_eq!(row.get::<_,i64>(4),1+committed);
                    observer.client().batch_execute("COMMIT").await.expect("observation complete");
                    proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    if case != "revoked" {
                        let original = Box::pin(services.grant().suspend(manager, operation_id, id, serde_json::from_value(body).expect("original confirmation"))).await.expect("original result or explicit retry after proven rollback");
                        assert_eq!(original.sequence(),2);
                    }
                    assert_eq!(services.grant().read_latest(&holder).await.expect("retained suspension").status(),Status::Suspended);
                    assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.suspensions)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=2 AND NOT EXISTS(SELECT 1 FROM access.terminations) AND EXISTS(SELECT 1 FROM access.suspensions suspension JOIN access.grants state ON state.id=suspension.grant_id JOIN identity.persons person ON person.id=suspension.person_id WHERE suspension.operation_id=$1 AND suspension.sequence=2 AND state.sequence=2 AND state.suspension_id=suspension.id AND person.sequence=1 AND octet_length(suspension.seal)=32 AND suspension.seal_key_id=0) AND EXISTS(SELECT 1 FROM access.completions WHERE family='grant_suspension' AND operation_id=$1)", &[&operation_id]).await.expect("one original suspension after restart and replay").get::<_,bool>(0));
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("suspension recovery deadline");
            }
        }));
    }

    #[test]
    fn suspension_preserves_membership_and_identity_and_replays_after_exit_and_rejoining() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (manager, holder, candidate) = Box::pin(participants(&database, &services, &mut supplier)).await;
                let body = confirmation(&candidate, "13900000000");
                let id = candidate.grant_id();
                assert!(matches!(services.grant().suspension_candidate(&holder, "13800000000".parse().expect("mobile")).await, Err(Error::Denied)), "institution authority is not domain suspension authority");
                for mobile in ["13800000000", "13700000000"] {
                    assert!(matches!(services.grant().suspension_candidate(&manager, mobile.parse().expect("mobile")).await, Err(Error::Absent)));
                }
                assert!(matches!(Box::pin(services.grant().suspend(holder.clone(), operation(), id, serde_json::from_value(body.clone()).expect("confirmation"))).await, Err(Error::Denied)));
                for field in ["mobile", "personId", "revisionId"] {
                    let mut changed = body.clone();
                    changed["target"][field] = if field == "mobile" { json!("13700000000") } else { json!(Uuid::new_v4()) };
                    assert!(matches!(Box::pin(services.grant().suspend(manager.clone(), operation(), id, serde_json::from_value(changed).expect("stale confirmation"))).await, Err(Error::TargetChanged)), "{field}");
                }
                let unknown: grant::Id = serde_json::from_value(json!(Uuid::new_v4())).expect("other grant");
                assert!(matches!(Box::pin(services.grant().suspend(manager.clone(), operation(), unknown, serde_json::from_value(body.clone()).expect("confirmation"))).await, Err(Error::TargetChanged)));
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let snapshot = "SELECT jsonb_build_object('accounts',(SELECT jsonb_agg(to_jsonb(account) ORDER BY id) FROM identity.accounts account),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session),'holders',(SELECT jsonb_agg(to_jsonb(holder) ORDER BY id) FROM access.holders holder),'memberships',(SELECT jsonb_agg(jsonb_build_object('id',id,'person',person_id,'role',role_id,'assignment',assignment_id) ORDER BY id) FROM access.grants WHERE termination IS NULL))::text";
                let before: String = observer.client().query_one(snapshot, &[]).await.expect("unrelated facts before suspension").get(0);
                let operation_id = operation();
                let original = Box::pin(services.grant().suspend(manager.clone(), operation_id, id, serde_json::from_value(body.clone()).expect("confirmation"))).await.expect("suspend lower domain member");
                assert_eq!(original.grant_id(), id);
                assert_eq!(original.operation_id(), operation_id);
                assert_eq!(original.sequence(), 2);
                assert_eq!(original.operator().name(), "T*");
                assert_eq!(original.subject().revision_id(), candidate.identity().revision_id());
                assert_ne!(original.operator().person_id(), original.subject().person_id());
                let after: String = observer.client().query_one(snapshot, &[]).await.expect("unrelated facts after suspension").get(0);
                assert_eq!(before, after, "pause does not rewrite current membership or identity state");
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.suspensions)=1 AND NOT EXISTS(SELECT 1 FROM access.terminations) AND EXISTS(SELECT 1 FROM access.suspensions suspension JOIN access.grants state ON state.id=suspension.grant_id JOIN identity.persons person ON person.id=suspension.person_id WHERE suspension.id=$1 AND suspension.operation_id=$2 AND suspension.grant_id=$3 AND suspension.sequence=2 AND state.sequence=2 AND state.suspension_id=suspension.id AND person.sequence=1 AND octet_length(suspension.seal)=32 AND suspension.seal_key_id=0) AND EXISTS(SELECT 1 FROM access.completions WHERE family='grant_suspension' AND operation_id=$2)", &[&original.id(),&operation_id,&id]).await.expect("independent paired facts before replay").get::<_,bool>(0));
                assert_eq!(services.grant().read_latest(&holder).await.expect("holder reads suspended grant").status(), Status::Suspended);
                services.grant().read_role(&holder).await.expect("suspension retains role projection");
                services.current().read(&holder).await.expect("original session remains usable");
                services.appointment().candidate(&holder, "13800000000".parse().expect("mobile")).await.expect("institution authority survives domain suspension");
                let current = services.grant().candidate(&manager, Entry::Domain, "13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate retains suspension status");
                assert_eq!(current.grant().expect("current grant").status, Status::Suspended);
                assert!(matches!(Box::pin(services.grant().create(manager.clone(), operation(), admission(&current, "domain", "13900000000", 0))).await, Err(Error::AlreadyAuthorized)), "suspension still occupies the one current grant");
                assert!(matches!(Box::pin(services.grant().suspend(manager.clone(), operation(), id, serde_json::from_value(body.clone()).expect("confirmation"))).await, Err(Error::StateConflict)));
                for field in ["mobile", "accountId", "personId", "revisionId"] {
                    let mut changed = body.clone();
                    changed["target"][field] = if field == "mobile" { json!("13700000000") } else { json!(Uuid::new_v4()) };
                    assert!(matches!(Box::pin(services.grant().suspend(manager.clone(), operation_id, id, serde_json::from_value(changed).expect("other input"))).await, Err(Error::Mismatch)), "original {field}");
                }
                Box::pin(services.grant().terminate(holder.clone(), operation(), id)).await.expect("suspended normal-account holder can exit");
                let candidate = services.grant().candidate(&manager, Entry::Domain, "13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("rejoin confirmation");
                let (replacement, _) = Box::pin(services.grant().create(manager.clone(), operation(), admission(&candidate, "domain", "13900000000", 0))).await.expect("new participation");
                assert_eq!(replacement.sequence(), 1);
                let correction = services.correction().candidate_by_mobile(&holder, "13800000000".parse().expect("manager mobile")).await.expect("manager correction confirmation");
                Box::pin(services.correction().correct(holder.clone(), operation(), serde_json::from_value(json!({"target":correction.target(),"name":"Changed Manager","idNumber":"00000020000229007X"})).expect("correction"))).await.expect("later operator correction");
                let replay = Box::pin(services.grant().suspend(manager.clone(), operation_id, id, serde_json::from_value(body.clone()).expect("original input"))).await.expect("original result after rejoin and correction");
                assert_eq!(replay.id(), original.id());
                assert_eq!(replay.suspended_at(), original.suspended_at());
                assert_eq!(replay.operator().revision_id(), original.operator().revision_id());
                assert_eq!(replay.operator().name(), "T*");
                assert!(matches!(Box::pin(services.grant().suspend(manager.clone(), operation(), id, serde_json::from_value(body.clone()).expect("old grant confirmation"))).await, Err(Error::TargetChanged)));
                assert_eq!(services.grant().read_latest(&holder).await.expect("new grant remains active").status(), Status::Active);
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.suspensions)=1 AND (SELECT count(*) FROM access.terminations)=1 AND EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND termination IS NULL AND sequence=1) AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$2 AND sequence=1)", &[&replacement.id(),&candidate.identity().person_id()]).await.expect("old replay preserves new grant and sequence").get::<_,bool>(0));
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let replay = Box::pin(services.grant().suspend(manager.clone(), operation_id, id, serde_json::from_value(body).expect("original input"))).await.expect("restart retains original proof");
                assert_eq!(replay.id(), original.id());
                let identity = services.current().read(&manager).await.expect("current manager");
                Box::pin(services.cancellation().cancel_account(holder, operation(), manager.account_id(), serde_json::from_value(json!({"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13800000000"})).expect("cancellation confirmation"))).await.expect("operator cancellation retains suspension history");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.close(); services.drain().await.expect("retained fixed operator survives restart"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("suspension lifecycle deadline");
        }));
    }
}
