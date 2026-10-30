//! Resume the exact unresolved suspension of a current Grant.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::time::timeout_at;
use tokio_postgres::Client;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    access::{
        grant::{
            self, CommandError, Error, Service,
            lifecycle::{self, Kind},
            member, suspension,
        },
        role,
    },
    history,
    identity::{
        account,
        current::{self, Actor},
        mobile::Mobile,
        operation,
        person::revision,
    },
    kernel::{Authorization, Command, completion, operation::Family, tx::Tx},
};

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = grant::resumption::Input)]
pub struct Input {
    target: member::Target,
}

pub struct Candidate {
    identity: account::Subject,
    suspension_id: suspension::Id,
}

impl Candidate {
    pub fn identity(&self) -> &account::Subject {
        &self.identity
    }
    pub fn suspension_id(&self) -> suspension::Id {
        self.suspension_id
    }
}

pub struct Resumption {
    id: suspension::Id,
    grant_id: grant::Id,
    subject: revision::Identity,
    operator: revision::Identity,
    resumed_at: DateTime<Utc>,
    sequence: i64,
    operation_id: operation::Id,
}

impl Resumption {
    pub fn id(&self) -> suspension::Id {
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
    pub fn resumed_at(&self) -> DateTime<Utc> {
        self.resumed_at
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
    resumptions: Vec<Resumption>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn resumptions(&self) -> &[Resumption] {
        &self.resumptions
    }
}

impl Resumption {
    fn from_fact(fact: lifecycle::Fact) -> Result<Self, Error> {
        let Kind::Resumption(id) = fact.kind else {
            return Err(Error::Facts {
                rule: "resumption delivery retains its exact record kind",
            });
        };
        Ok(Self {
            id,
            grant_id: fact.grant_id,
            subject: fact.subject,
            operator: fact.operator,
            resumed_at: fact.happened_at,
            sequence: fact.sequence,
            operation_id: fact.operation_id,
        })
    }
}

impl Service {
    pub async fn list_resumptions(
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
                        self.fetch_resumptions(client, &roles, filter, now).await
                    }
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    async fn fetch_resumptions(
        &self,
        client: &Client,
        roles: &role::Reader,
        filter: &grant::Filter,
        now: DateTime<Utc>,
    ) -> Result<List, Error> {
        let (count, facts) =
            lifecycle::page(self, client, roles, filter, "resumption", now).await?;
        let resumptions = facts
            .into_iter()
            .map(Resumption::from_fact)
            .collect::<Result<_, _>>()?;
        Ok(List { count, resumptions })
    }
    pub async fn resumption_candidate(
        &self,
        actor: &Actor,
        mobile: Mobile,
    ) -> Result<Candidate, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let mobile = &mobile;
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    async move {
                        let (manager, person_id, _, now) = self
                            .authorize_member(
                                client,
                                &roles,
                                actor,
                                role::Code::WorkforceSuspension,
                            )
                            .await?;
                        let account_id = self
                            .accounts
                            .at(view)
                            .find_by_mobile(client, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        let member = Self::fetch_managed_member(
                            client, &roles, &manager, person_id, account_id, now,
                        )
                        .await?;
                        let identity = self
                            .accounts
                            .at(view)
                            .find_by_id(client, account_id, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        let suspension_id =
                            suspension::find_open_by_grant(client, &roles, member.id(), now)
                                .await?
                                .ok_or(Error::StateConflict)?;
                        Ok(Candidate {
                            identity,
                            suspension_id,
                        })
                    }
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    pub async fn resume(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        suspension_id: suspension::Id,
        input: Input,
    ) -> Result<Resumption, Error> {
        Resumption::from_fact(
            self.app
                .command(
                    operation_id,
                    Lifecycle(lifecycle::Command {
                        service: self.clone(),
                        actor,
                        action: lifecycle::Action::Resume {
                            id: suspension_id,
                            target: input.target,
                        },
                    }),
                )
                .await?,
        )
    }
}

fn accepts(action: &str, target: &str) -> bool {
    action == "resume" && Uuid::parse_str(target).is_ok_and(|id| id.to_string() == target)
}
pub(super) const COMPLETIONS: completion::Template =
    completion::Template::personal(Family::GrantResumption, accepts);

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
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::{
            grant::{
                Entry, Error, Status,
                suspension::tests::{admission, confirmation, participants, wait_for_lock},
            },
            role::tests::start,
        },
        identity::{
            authentication::{Database, Supplier, server},
            operation,
        },
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    #[test]
    fn resumption_confirms_current_identity_without_restoring_account_access() {
        use crate::identity::{authentication, current};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (manager, institution, _) = Box::pin(participants(&database, &services, &mut supplier)).await;
                let message_id = authentication::session::tests::message(&database, services.authentication(), &mut supplier, "13700000000").await;
                let authentication::session::Receipt::Registration(credential) = Box::pin(services.authentication().create_session(authentication::session::tests::input(Uuid::new_v4(), message_id, "13700000000", authentication::session::tests::CODE))).await.expect("R1") else { panic!("registration credential"); };
                let registered = Box::pin(services.registration().register_input_for_test(crate::registration::Input::new(operation(), credential.id(), credential.token().text().parse().expect("proof"), "Other Member".parse().expect("name"), "000000200002290088".parse().expect("id number")))).await.expect("R2");
                let target = services.current().authenticate(current::Proof::Cookie { id: registered.id(), token: registered.token().text().parse().expect("token") }).await.expect("target actor");
                let candidate = services.grant().candidate(&manager, Entry::Domain, "13700000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("admission confirmation");
                let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation(), admission(&candidate, "domain", "13700000000", 0))).await.expect("ordinary member");
                let candidate = services.grant().suspension_candidate(&manager, "13700000000".parse().expect("mobile")).await.expect("suspension confirmation");
                let stale = confirmation(&candidate, "13700000000");
                let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), grant.id(), serde_json::from_value(stale.clone()).expect("pause input"))).await.expect("domain pause");
                let identity = services.correction().candidate_by_mobile(&institution, "13700000000".parse().expect("mobile")).await.expect("current identity");
                Box::pin(services.correction().correct(institution.clone(), operation(), serde_json::from_value(json!({"target":{"personId":identity.target().person_id(),"revisionId":identity.target().revision_id()},"name":"Revised Member","idNumber":"000000200002290088"})).expect("correction input"))).await.expect("real identity correction");
                assert!(matches!(Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(stale).expect("stale confirmation"))).await, Err(Error::TargetChanged)));
                let candidate = services.grant().resumption_candidate(&manager, "13700000000".parse().expect("mobile")).await.expect("corrected identity and exact pause");
                assert_eq!(candidate.suspension_id(), pause.id());
                assert_ne!(candidate.identity().revision_id(), pause.subject().revision_id());
                let identity = candidate.identity();
                let body = json!({"target":{"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13700000000"}});
                let account_pause = Box::pin(services.suspension().suspend(institution, operation(), identity.id(), serde_json::from_value(json!({"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13700000000"})).expect("account confirmation"))).await.expect("account pause revokes old sessions");
                services.grant().resumption_candidate(&manager, "13700000000".parse().expect("mobile")).await.expect("paused account remains manageable");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let sql = "SELECT jsonb_build_object('account',(SELECT to_jsonb(account) FROM identity.accounts account WHERE id=$1),'holder',(SELECT to_jsonb(holder) FROM access.holders holder WHERE id=$1),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session WHERE account_id=$1),'revocations',(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM identity.revocations revocation WHERE account_id=$1),'handoffs',(SELECT jsonb_agg(to_jsonb(handoff) ORDER BY id) FROM identity.handoffs handoff WHERE id IN (SELECT id FROM identity.sessions WHERE account_id=$1)),'membership',(SELECT jsonb_build_object('person',person_id,'role',role_id,'assignment',assignment_id) FROM access.grants WHERE id=$2 AND termination IS NULL),'assignments',(SELECT jsonb_agg(to_jsonb(assignment) ORDER BY id) FROM access.assignments assignment WHERE grant_id=$2))::text";
                let before: String = observer.client().query_one(sql, &[&identity.id(), &grant.id()]).await.expect("independent state before resume").get(0);
                let original = operation();
                let resumed = Box::pin(services.grant().resume(manager.clone(), original, pause.id(), serde_json::from_value(body.clone()).expect("current confirmation"))).await.expect("domain resume despite account pause");
                assert_eq!(resumed.sequence(), 3);
                assert_eq!(resumed.subject().revision_id(), identity.revision_id());
                let after: String = observer.client().query_one(sql, &[&identity.id(), &grant.id()]).await.expect("independent state before replay").get(0);
                assert_eq!(before, after, "domain resumption preserves account, sessions and membership");
                assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.resumptions WHERE id=$1 AND sequence=3) AND EXISTS(SELECT 1 FROM identity.suspensions WHERE id=$2) AND NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE id=$2) AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$3 AND sequence=2)", &[&pause.id(), &account_pause.id(), &identity.person_id()]).await.expect("separate permanent state facts").get::<_,bool>(0));
                assert!(matches!(services.current().read(&target).await, Err(current::Error::Invalid)), "domain resume does not restore the revoked session");
                Box::pin(services.grant().terminate_member(manager.clone(), operation(), grant.id(), serde_json::from_value(body.clone()).expect("current target"))).await.expect("later domain termination");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let replay = Box::pin(services.grant().resume(manager, original, pause.id(), serde_json::from_value(body).expect("retained input"))).await.expect("fixed result after later termination and restart");
                assert_eq!(replay.subject().revision_id(), identity.revision_id());
                assert_eq!(replay.resumed_at(), resumed.resumed_at());
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("identity and account boundary deadline");
        }));
    }

    #[test]
    fn resumption_replay_retains_original_after_parent_loss_and_rejects_identity_loss() {
        use fixture::database_proxy::Proxy;

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for missing in ["suspension","operator"] {
                timeout(Duration::from_secs(40),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;
                    let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                    let(context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                    let(manager,_,candidate)=Box::pin(participants(&database,&services,&mut supplier)).await;
                    let body=confirmation(&candidate,"13900000000");
                    let pause=Box::pin(services.grant().suspend(manager.clone(),operation(),candidate.grant_id(),serde_json::from_value(body.clone()).expect("pause input"))).await.expect("actual suspension");
                    let original=operation();
                    let owner=database.connections("postgres").recover().await.expect("isolated damage owner");
                    if missing=="operator" {
                        let error=Box::pin(crate::recovery::after_revision_loss(&database,&proxy,&manager,
                            services.grant().resume(manager.clone(),original,pause.id(),serde_json::from_value(body.clone()).expect("resume input"))))
                            .await.err().expect("actual lost-commit recovery cannot confirm absence");
                        let Error::Command(crate::kernel::Error::Unknown(fault)) = error else { panic!("damaged fixed identity preserves an unknown commit outcome"); };
                        assert!(std::iter::once((fault.stage(), fault.kind())).chain(fault.later()).any(|(_,kind)| matches!(kind,crate::kernel::FaultKind::Revision(error) if error.unreliable())), "{fault:?}");
                        assert!(services.ready().await.is_err());
                    }else{
                        let resumed = Box::pin(services.grant().resume(manager.clone(),original,pause.id(),serde_json::from_value(body.clone()).expect("resume input"))).await.expect("stored resumption");
                        owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated missing parent");
                        assert_eq!(owner.client().execute("DELETE FROM access.suspensions WHERE id=$1",&[&pause.id()]).await.expect("remove parent only"),1);
                        owner.client().batch_execute("COMMIT").await.expect("damage committed");
                        let replay=Box::pin(services.grant().resume(manager.clone(),original,pause.id(),serde_json::from_value(body.clone()).expect("original request"))).await.expect("the sealed resumption does not reconstruct its parent");
                        assert_eq!(replay.id(), resumed.id());
                        assert_eq!(replay.resumed_at(), resumed.resumed_at());
                        assert_eq!(replay.sequence(), resumed.sequence());
                    }
                    let verified = services.grant().verify(owner.client()).await;
                    assert!(verified.is_err(),"startup rejects missing {missing}");
                    assert!(owner.client().query_one("SELECT (SELECT count(*) FROM access.resumptions WHERE id=$1 AND operation_id=$2)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=2 AND EXISTS(SELECT 1 FROM access.grants WHERE person_id=$3 AND sequence=3 AND suspension_id IS NULL) AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$3 AND sequence=1)",&[&pause.id(),&original,&candidate.identity().person_id()]).await.expect("original resumption remains without reconstruction or duplicate writes").get::<_,bool>(0));
                    owner.terminate().await.expect("owner ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("missing resumption reference deadline");
            }
        }));
    }

    #[test]
    fn resumption_history_survives_cancellation_and_rejects_damaged_older_facts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["cancellation","missing","future","sequence","overlap","key"] {
                timeout(Duration::from_secs(45),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;
                    let mut supplier=Supplier::new().await;
                    let(context,services)=start(database.connections(&database.user),&supplier).await;
                    let(manager,holder,candidate)=Box::pin(participants(&database,&services,&mut supplier)).await;
                    let body=confirmation(&candidate,"13900000000");let original=operation();
                    let pause=Box::pin(services.grant().suspend(manager.clone(),operation(),candidate.grant_id(),serde_json::from_value(body.clone()).expect("pause input"))).await.expect("pause");
                    let resumed=Box::pin(services.grant().resume(manager.clone(),original,pause.id(),serde_json::from_value(body.clone()).expect("resume input"))).await.expect("resume");
                    if case=="cancellation" {
                        Box::pin(services.cancellation().cancel(holder,operation(),serde_json::from_value(json!({"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id()})).expect("self cancellation"))).await.expect("cancellation after resume");
                        let observer=database.connections(&database.user).recover().await.expect("independent observer");
                        assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.resumptions WHERE id=$1 AND sequence=3) AND EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$2 AND sequence=2) AND EXISTS(SELECT 1 FROM access.terminations ended JOIN access.grants state ON state.id=ended.id WHERE ended.id=$3 AND ended.mode='cancellation' AND ended.sequence=4 AND state.sequence=4 AND state.termination='cancellation' AND ended.cancellation_id=(SELECT id FROM identity.cancellations WHERE person_id=$2)) AND NOT EXISTS(SELECT 1 FROM access.grants WHERE id=$3 AND termination IS NULL)",&[&pause.id(),&candidate.identity().person_id(),&candidate.grant_id()]).await.expect("cancellation preserves exact history with independent personal and Grant order").get::<_,bool>(0));
                        services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                        let(context,services)=start(database.connections(&database.user),&supplier).await;
                        assert_eq!(Box::pin(services.grant().resume(manager,original,pause.id(),serde_json::from_value(body).expect("original input"))).await.expect("retained completion after cancellation and restart").resumed_at(),resumed.resumed_at());
                        observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                    }else{
                        Box::pin(services.grant().suspend(manager.clone(),operation(),candidate.grant_id(),serde_json::from_value(body.clone()).expect("second pause"))).await.expect("later pause");
                        Box::pin(services.grant().terminate(holder.clone(),operation(),candidate.grant_id())).await.expect("exit");
                        let confirmed=services.grant().candidate(&manager,Entry::Domain,"13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("rejoin confirmation");
                        let(replacement,_)=Box::pin(services.grant().create(manager.clone(),operation(),admission(&confirmed,"domain","13900000000",0))).await.expect("new grant");
                        let owner=database.connections("postgres").recover().await.expect("isolated damage owner");
                        owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated fault");
                        if case=="key" {owner.client().batch_execute("INSERT INTO security.keys(id,fingerprint,active) SELECT 1,fingerprint,false FROM security.keys WHERE id=0").await.expect("retained key metadata");}
                        let sql=match case {"missing"=>"DELETE FROM access.resumptions WHERE id=$1","future"=>"UPDATE access.resumptions SET resumed_at=clock_timestamp()+interval '1 day' WHERE id=$1","sequence"=>"UPDATE access.resumptions SET sequence=2 WHERE id=$1","overlap"=>"UPDATE access.resumptions SET resumed_at=(SELECT max(later.suspended_at)+interval '1 microsecond' FROM access.suspensions original JOIN access.suspensions later ON later.grant_id=original.grant_id WHERE original.id=$1) WHERE id=$1","key"=>"UPDATE access.resumptions SET seal_key_id=1 WHERE id=$1",_=>unreachable!()};
                        assert_eq!(owner.client().execute(sql,&[&pause.id()]).await.expect("damage only retained resumption"),1);owner.client().batch_execute("COMMIT").await.expect("fault committed");
                        if case=="key" {assert!(services.grant().verify(owner.client()).await.is_err(),"resumption alone retains its old material");}
                        else {let personal=crate::identity::person::verify(owner.client(),services.app().keys(),chrono::Utc::now()).await;let access=services.grant().verify(owner.client()).await;assert!(personal.is_err()||access.is_err(),"startup rejects {case}");if case=="overlap" {assert!(personal.is_ok(),"chronology damage does not rely on a sequence hole");}}
                        Box::pin(services.grant().terminate(holder,operation(),replacement.id())).await.expect("new Grant does not consume an old resumption");
                        services.ready().await.expect("unread old damage does not close admission");
                        let error=Box::pin(services.grant().resume(manager,original,pause.id(),serde_json::from_value(body).expect("original input"))).await.err().expect("replay authenticates its original resumption");
                        assert_eq!(error.failure(),crate::access::grant::Failure::Unavailable,"{case}: {error:?}");assert!(services.ready().await.is_err());
                        assert!(owner.client().query_one("SELECT EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND termination='self' AND sequence=2) AND EXISTS(SELECT 1 FROM access.terminations WHERE id=$1 AND sequence=2) AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$2 AND sequence=1)",&[&replacement.id(),&candidate.identity().person_id()]).await.expect("new Grant ended independently and replay did not repair history").get::<_,bool>(0));
                        owner.terminate().await.expect("owner ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                    }
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("retained resumption history deadline");
            }
        }));
    }

    #[test]
    fn resumption_serializes_competitors_and_rechecks_ended_participants() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["manager exits","holder exits","duplicate","different operation","later pause"] {
                timeout(Duration::from_secs(45),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;
                    let mut supplier=Supplier::new().await;
                    let(context,services)=start(database.connections(&database.user),&supplier).await;
                    let(manager,holder,candidate)=Box::pin(participants(&database,&services,&mut supplier)).await;
                    let body=confirmation(&candidate,"13900000000");
                    let pause=Box::pin(services.grant().suspend(manager.clone(),operation(),candidate.grant_id(),serde_json::from_value(body.clone()).expect("pause input"))).await.expect("open suspension");
                    let grant_id=candidate.grant_id();let suspension_id=pause.id();
                    let target=if case=="manager exits" {services.grant().read_latest(&manager).await.expect("manager grant").id()}else{grant_id};
                    let blocker=database.connections("postgres").recover().await.expect("isolated blocker");
                    blocker.client().batch_execute("BEGIN").await.expect("block participant");
                    blocker.client().query_one("SELECT id FROM access.grants WHERE id=$1 FOR UPDATE",&[&target]).await.expect("grant lock");
                    let pid:i32=blocker.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("blocker PID").get(0);
                    let observer=database.connections(&database.user).recover().await.expect("independent observer");
                    let first=operation();let service=services.grant().clone();let actor=if case=="manager exits"{manager.clone()}else{holder.clone()};let operator=manager.clone();let input=serde_json::from_value(body.clone()).expect("first input");
                    let preceding=tokio::spawn(async move {
                        if case.ends_with("exits"){service.terminate(actor,first,target).await.map(|_|())}
                        else{service.resume(operator,first,suspension_id,input).await.map(|_|())}
                    });
                    wait_for_lock(observer.client(),pid,"SELECT id FROM access.grants WHERE id=$1 FOR UPDATE").await;
                    let operation_id=if case=="duplicate"{first}else{operation()};
                    let service=services.grant().clone();let input=body.clone();let actor=manager.clone();
                    let mut pending=tokio::spawn(async move {
                        if case=="later pause"{service.suspend(actor,operation_id,grant_id,serde_json::from_value(input).expect("next pause input")).await.map(|value|value.sequence())}
                        else{service.resume(actor,operation_id,suspension_id,serde_json::from_value(input).expect("next resume input")).await.map(|value|value.sequence())}
                    });
                    if case=="duplicate" {assert!(timeout(Duration::from_millis(50),&mut pending).await.is_err(),"duplicate waits for the accepted original");}
                    else {wait_for_lock(observer.client(),pid,if case=="manager exits"{"SELECT id FROM access.grants WHERE id=$1 FOR SHARE"}else{"SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE"}).await;}
                    blocker.client().batch_execute("COMMIT").await.expect("release participant");
                    timeout(Duration::from_secs(5),preceding).await.expect("first deadline").expect("first task").expect("first transition");
                    let result=timeout(Duration::from_secs(5),pending).await.expect("second deadline").expect("second task");
                    match case {"manager exits"=>assert!(matches!(result,Err(Error::Denied))),"holder exits"=>assert!(matches!(result,Err(Error::Absent))),"duplicate"=>assert_eq!(result.expect("same original"),3),"later pause"=>assert_eq!(result.expect("new pause after resume"),4),_=>assert!(matches!(result,Err(Error::StateConflict)))}
                    let completed=i64::from(!case.ends_with("exits"));
                    let row=observer.client().query_one("SELECT (SELECT count(*) FROM access.resumptions),(SELECT count(*) FROM access.suspensions),(SELECT count(*) FROM access.terminations),(SELECT count(*) FROM access.grants WHERE termination IS NULL)",&[]).await.expect("independent ordered outcomes");
                    assert_eq!(row.get::<_,i64>(0),completed);assert_eq!(row.get::<_,i64>(1),1+i64::from(case=="later pause"));assert_eq!(row.get::<_,i64>(2),1-completed);assert_eq!(row.get::<_,i64>(3),1+completed);
                    services.current().read(&holder).await.expect("state changes preserve account session");
                    observer.terminate().await.expect("observer ended");blocker.terminate().await.expect("blocker ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("resumption coordination deadline");
            }
        }));
    }

    #[test]
    fn resumption_sql_requires_exact_pairing_and_complete_grant_transition() {
        use tokio_postgres::error::SqlState;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(45),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let (manager, _, candidate) =
                        Box::pin(participants(&database, &services, &mut supplier)).await;
                    let body = confirmation(&candidate, "13900000000");
                    let pause = Box::pin(services.grant().suspend(
                        manager.clone(),
                        operation(),
                        candidate.grant_id(),
                        serde_json::from_value(body.clone()).expect("pause input"),
                    ))
                    .await
                    .expect("actual pause");
                    let operator = services
                        .current()
                        .read(&manager)
                        .await
                        .expect("current manager");
                    let writer = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("runtime writer");
                    crate::access::grant::tests::sql_transition(
                        &database,
                        &services,
                        candidate.grant_id(),
                        operator.revision_id(),
                        crate::access::grant::tests::SqlAction::Resume,
                    )
                    .await;
                    for sql in [
                        "UPDATE access.resumptions SET seal=seal",
                        "DELETE FROM access.resumptions",
                    ] {
                        assert_eq!(
                            writer
                                .client()
                                .batch_execute(sql)
                                .await
                                .expect_err("runtime cannot rewrite history")
                                .code(),
                            Some(&SqlState::INSUFFICIENT_PRIVILEGE)
                        );
                    }
                    let resumed = Box::pin(services.grant().resume(
                        manager,
                        operation(),
                        pause.id(),
                        serde_json::from_value(body).expect("same valid confirmation"),
                    ))
                    .await
                    .expect("valid path remains usable");
                    assert_eq!(resumed.sequence(), 3);
                    writer.terminate().await.expect("writer ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    assert!(
                        timeout(Duration::from_millis(30), supplier.request())
                            .await
                            .is_err()
                    );
                    supplier.stop().await;
                }),
            )
            .await
            .expect("resumption SQL deadline");
        }));
    }

    #[test]
    fn resumption_recovers_its_exact_fact_and_retains_accepted_work() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::{authentication, current};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["lost", "rollback", "unknown", "cleanup", "cancelled", "revoked"] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let (manager, holder, candidate) = Box::pin(participants(&database, &services, &mut supplier)).await;
                    let body = confirmation(&candidate,"13900000000");
                    let pause = Box::pin(services.grant().suspend(manager.clone(),operation(),candidate.grant_id(),serde_json::from_value(body.clone()).expect("pause input"))).await.expect("open pause");
                    let id=pause.id();let operation_id=operation();
                    if case=="revoked" {
                        let original=crate::recovery::after_revocation(&database,&proxy,services.revocation(),&manager,
                            Box::pin(services.grant().resume(manager.clone(),operation_id,id,serde_json::from_value(body.clone()).expect("confirmation"))))
                            .await.expect("later revocation preserves accepted completion");
                        assert_eq!(original.id(),id);
                        assert!(matches!(Box::pin(services.grant().resume(manager.clone(),operation_id,id,serde_json::from_value(body.clone()).expect("new request"))).await,Err(Error::Current(current::Error::Invalid))));
                        services.close();services.drain().await.expect("drained");
                    } else {
                        proxy.arm(match case {"lost"=>Fault::LostCommit,"rollback"=>Fault::RolledBack,"unknown"=>Fault::UnknownCommit,"cleanup"|"cancelled"=>Fault::DelayedCommit,_=>unreachable!()});
                        let service=services.grant().clone();let actor=manager.clone();let input=serde_json::from_value(body.clone()).expect("confirmation");
                        let waiter=tokio::spawn(async move {service.resume(actor,operation_id,id,input).await});
                        timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("original commit actually intercepted");
                        if case=="cleanup" {proxy.arm(Fault::ReadCommitTermination);}
                        if case=="cancelled" {
                            waiter.abort();assert!(matches!(waiter.await,Err(error) if error.is_cancelled()));services.close();
                            assert!(timeout(Duration::from_millis(50),services.drain()).await.is_err(),"wait cancellation retains original work");
                            proxy.release();services.drain().await.expect("accepted work drained");
                        } else {
                            proxy.release();let result=timeout(Duration::from_secs(15),waiter).await.expect("bounded outcome").expect("waiter");
                            match case {"rollback"=>assert!(matches!(result,Err(Error::Command(crate::kernel::Error::Unavailable(ref fault))) if fault.stage()==crate::kernel::Stage::Commit)),"unknown"=>assert!(matches!(result,Err(Error::Command(crate::kernel::Error::Unknown(_))))),_=>assert_eq!(result.expect("original resumption").sequence(),3)}
                            services.close();services.drain().await.expect("drained");
                        }
                    }
                    context.stop().await.expect("stopped");
                    let observer=database.connections(&database.user).recover().await.expect("independent observer");
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT id FROM access.grants WHERE id=$1 FOR UPDATE",&[&candidate.grant_id()]).await.expect("original backend ended");
                    let committed=i64::from(case!="rollback");
                    let row=observer.client().query_one("SELECT (SELECT count(*) FROM access.suspensions),(SELECT count(*) FROM access.resumptions),(SELECT count(*) FROM access.grants WHERE termination IS NULL),sequence FROM access.grants WHERE id=$1",&[&candidate.grant_id()]).await.expect("durable state before replay");
                    assert_eq!(row.get::<_,i64>(0),1);assert_eq!(row.get::<_,i64>(1),committed);assert_eq!(row.get::<_,i64>(2),2);assert_eq!(row.get::<_,i64>(3),2+committed);
                    observer.client().batch_execute("COMMIT").await.expect("observation ended");proxy.stop().await;
                    let (context,services)=start(database.connections(&database.user),&supplier).await;
                    if case!="revoked" {assert_eq!(Box::pin(services.grant().resume(manager,operation_id,id,serde_json::from_value(body).expect("original input"))).await.expect("original or explicit retry after rollback").sequence(),3);}
                    assert_eq!(services.grant().read_latest(&holder).await.expect("active projection").status(),Status::Active);
                    assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.resumptions)=1 AND EXISTS(SELECT 1 FROM access.resumptions resumption JOIN access.grants state ON state.id=resumption.grant_id JOIN identity.persons person ON person.id=resumption.person_id WHERE resumption.id=$1 AND resumption.operation_id=$2 AND resumption.sequence=3 AND state.sequence=3 AND state.suspension_id IS NULL AND person.sequence=1 AND octet_length(resumption.seal)=32 AND resumption.seal_key_id=0) AND EXISTS(SELECT 1 FROM access.completions WHERE family='grant_resumption' AND operation_id=$2)",&[&id,&operation_id]).await.expect("one original completion after replay").get::<_,bool>(0));
                    observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("resumption recovery deadline");
            }
        }));
    }

    #[test]
    fn resumption_retains_exact_history_across_repeated_suspension_and_rejoining() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (manager, holder, candidate) = Box::pin(participants(&database, &services, &mut supplier)).await;
                let body = confirmation(&candidate, "13900000000");
                assert!(matches!(services.grant().resumption_candidate(&manager, "13900000000".parse().expect("mobile")).await, Err(Error::StateConflict)));
                let pause_operation = operation();
                let pause = Box::pin(services.grant().suspend(manager.clone(), pause_operation, candidate.grant_id(), serde_json::from_value(body.clone()).expect("pause"))).await.expect("first suspension");
                let confirmed = services.grant().resumption_candidate(&manager, "13900000000".parse().expect("mobile")).await.expect("exact open suspension");
                assert_eq!(confirmed.suspension_id(), pause.id());
                assert_eq!(confirmed.identity().id(), candidate.identity().id());
                let original = operation();
                let resumed = Box::pin(services.grant().resume(manager.clone(), original, pause.id(), serde_json::from_value(body.clone()).expect("resume"))).await.expect("resumed");
                assert_eq!(resumed.id(), pause.id());
                assert_eq!(resumed.grant_id(), candidate.grant_id());
                assert_eq!(resumed.sequence(), 3);
                assert!(resumed.resumed_at() >= pause.suspended_at());
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let row = observer.client().query_one("SELECT resumption.sequence,person.sequence AS person_sequence,(SELECT count(*) FROM access.resumptions WHERE id=$1) AS resumptions,(SELECT count(*) FROM access.grants WHERE id=$2 AND termination IS NULL AND suspension_id IS NULL AND sequence=3) AS members,(SELECT count(*) FROM access.assignments WHERE grant_id=$2) AS assignments,EXISTS(SELECT 1 FROM identity.accounts WHERE id=$3) AS account,EXISTS(SELECT 1 FROM access.holders WHERE id=$3) AS holder,EXISTS(SELECT 1 FROM identity.sessions WHERE id=$4) AS session,NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$4) AS unrevoked FROM access.resumptions resumption JOIN identity.persons person ON person.id=resumption.person_id WHERE resumption.id=$1", &[&pause.id(), &candidate.grant_id(), &candidate.identity().id(), &holder.session_id()]).await.expect("durable facts before any replay");
                assert_eq!(row.get::<_,i64>("sequence"), 3);
                assert_eq!(row.get::<_,i64>("person_sequence"), 1);
                for column in ["resumptions", "members", "assignments"] { assert_eq!(row.get::<_,i64>(column), 1, "{column}"); }
                for column in ["account", "holder", "session", "unrevoked"] { assert!(row.get::<_,bool>(column), "{column}"); }
                assert_eq!(services.grant().read_latest(&holder).await.expect("active projection").status(), Status::Active);
                assert!(matches!(Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(body.clone()).expect("repeat input"))).await, Err(Error::StateConflict)));
                let mut changed = body.clone(); changed["target"]["mobile"] = json!("13700000000");
                assert!(matches!(Box::pin(services.grant().resume(manager.clone(), original, pause.id(), serde_json::from_value(changed).expect("changed input"))).await, Err(Error::Mismatch)));
                let later = Box::pin(services.grant().suspend(manager.clone(), operation(), candidate.grant_id(), serde_json::from_value(body.clone()).expect("pause again"))).await.expect("second suspension");
                assert_ne!(later.id(), pause.id());
                assert_eq!(later.sequence(), 4);
                let replay = Box::pin(services.grant().resume(manager.clone(), original, pause.id(), serde_json::from_value(body.clone()).expect("original input"))).await.expect("old resumption");
                assert_eq!(replay.resumed_at(), resumed.resumed_at());
                assert_eq!(services.grant().read_latest(&holder).await.expect("new suspension remains").status(), Status::Suspended);
                assert!(matches!(Box::pin(services.grant().resume(manager.clone(), original, later.id(), serde_json::from_value(body.clone()).expect("changed suspension"))).await, Err(Error::Mismatch)));
                assert!(matches!(Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(body.clone()).expect("old suspension"))).await, Err(Error::StateConflict)));
                assert_eq!(Box::pin(services.grant().suspend(manager.clone(), pause_operation, candidate.grant_id(), serde_json::from_value(body.clone()).expect("original pause"))).await.expect("old pause still valid").id(), pause.id());
                Box::pin(services.grant().terminate(holder.clone(), operation(), candidate.grant_id())).await.expect("exit while suspended");
                assert!(matches!(Box::pin(services.grant().resume(manager.clone(), operation(), later.id(), serde_json::from_value(body.clone()).expect("ended target"))).await, Err(Error::Absent)));
                let next = services.grant().candidate(&manager, Entry::Domain, "13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("rejoin confirmation");
                let (next, _) = Box::pin(services.grant().create(manager.clone(), operation(), admission(&next, "domain", "13900000000", 0))).await.expect("new grant");
                assert_ne!(next.id(), candidate.grant_id());
                let replay = Box::pin(services.grant().resume(manager.clone(), original, pause.id(), serde_json::from_value(body.clone()).expect("retained original"))).await.expect("original after rejoin");
                assert_eq!(replay.sequence(), 3);
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.suspensions WHERE grant_id=$1)=2 AND (SELECT count(*) FROM access.resumptions WHERE person_id=$2)=1 AND EXISTS(SELECT 1 FROM access.grants WHERE id=$3 AND termination IS NULL AND sequence=1) AND EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND termination='self' AND sequence=5) AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$2 AND sequence=1)", &[&candidate.grant_id(), &candidate.identity().person_id(), &next.id()]).await.expect("old replay has no new effect").get::<_,bool>(0));
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                assert_eq!(Box::pin(services.grant().resume(manager, original, pause.id(), serde_json::from_value(body).expect("restart original"))).await.expect("restart history").resumed_at(), resumed.resumed_at());
                services.close(); services.drain().await.expect("restart drain"); context.stop().await.expect("restart stop"); observer.terminate().await.expect("observer stop");
                assert!(timeout(Duration::from_millis(40), supplier.request()).await.is_err(), "no supplier request through replay and stop"); supplier.stop().await;
            })).await.expect("resumption lifecycle deadline");
        }));
    }
}
