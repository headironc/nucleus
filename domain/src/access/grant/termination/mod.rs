//! End a Grant with its explicit manual or cancellation source.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::timeout_at;
use tokio_postgres::Client;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    access::{
        grant::{
            self, CommandError, Error, Filter, Id, Service,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = termination::Mode)]
pub enum Mode {
    #[serde(rename = "self")]
    Current,
    #[serde(rename = "manager")]
    Manager,
    #[serde(rename = "cancellation")]
    Cancellation,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = termination::Input)]
pub struct Input {
    target: member::Target,
}

pub struct Termination {
    id: Id,
    mode: Mode,
    subject: revision::Identity,
    operator: revision::Identity,
    terminated_at: DateTime<Utc>,
    sequence: i64,
    operation_id: operation::Id,
}

impl Termination {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn subject(&self) -> &revision::Identity {
        &self.subject
    }
    pub fn operator(&self) -> &revision::Identity {
        &self.operator
    }
    pub fn terminated_at(&self) -> DateTime<Utc> {
        self.terminated_at
    }
    pub fn sequence(&self) -> i64 {
        self.sequence
    }
    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
}

pub struct List {
    count: i64,
    terminations: Vec<Termination>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn terminations(&self) -> &[Termination] {
        &self.terminations
    }
}

impl Termination {
    fn from_fact(fact: lifecycle::Fact) -> Result<Self, Error> {
        let Kind::Termination(mode) = fact.kind else {
            return Err(Error::Facts {
                rule: "termination delivery retains its exact record kind",
            });
        };
        Ok(Self {
            id: fact.grant_id,
            mode,
            subject: fact.subject,
            operator: fact.operator,
            terminated_at: fact.happened_at,
            sequence: fact.sequence,
            operation_id: fact.operation_id,
        })
    }
}

impl Service {
    pub async fn list_terminations(&self, actor: &Actor, filter: Filter) -> Result<List, Error> {
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
                            role::Code::WorkforceTermination,
                        )
                        .await?;
                        self.fetch_terminations(client, &roles, filter, now).await
                    }
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    async fn fetch_terminations(
        &self,
        client: &Client,
        roles: &role::Reader,
        filter: &grant::Filter,
        now: DateTime<Utc>,
    ) -> Result<List, Error> {
        let (count, facts) =
            lifecycle::page(self, client, roles, filter, "termination", now).await?;
        let terminations = facts
            .into_iter()
            .map(Termination::from_fact)
            .collect::<Result<_, _>>()?;
        Ok(List {
            count,
            terminations,
        })
    }
    pub async fn termination_candidate(
        &self,
        actor: &Actor,
        mobile: Mobile,
    ) -> Result<member::Candidate, Error> {
        self.member_candidate(actor, mobile, role::Code::WorkforceTermination)
            .await
    }

    pub async fn terminate(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        id: Id,
    ) -> Result<Termination, Error> {
        Termination::from_fact(
            self.app
                .command(
                    operation_id,
                    Lifecycle(lifecycle::Command {
                        service: self.clone(),
                        actor,
                        action: lifecycle::Action::Terminate {
                            grant_id: id,
                            target: None,
                        },
                    }),
                )
                .await?,
        )
    }
    pub async fn terminate_member(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        id: Id,
        input: Input,
    ) -> Result<Termination, Error> {
        Termination::from_fact(
            self.app
                .command(
                    operation_id,
                    Lifecycle(lifecycle::Command {
                        service: self.clone(),
                        actor,
                        action: lifecycle::Action::Terminate {
                            grant_id: id,
                            target: Some(input.target),
                        },
                    }),
                )
                .await?,
        )
    }
}

fn accepts(action: &str, target: &str) -> bool {
    matches!(action, "leave" | "terminate")
        && Uuid::parse_str(target).is_ok_and(|id| id.to_string() == target)
}
pub(super) const COMPLETIONS: completion::Template =
    completion::Template::personal(Family::GrantTermination, accepts);

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
            appointment::tests::register,
            grant::{self, Candidate, Entry, Error, Id, Input, Status, member, termination::Mode},
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

    fn input(candidate: &Candidate, index: usize) -> Input {
        admission(candidate, "institution", "13800000000", index)
    }

    fn admission(candidate: &Candidate, entry: &str, mobile: &str, index: usize) -> Input {
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

    #[test]
    fn managed_termination_sql_pairs_grant_state_and_records_and_allows_suspended_targets() {
        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(50), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (manager, institution, _) = Box::pin(managed_member(&database, &services, &mut supplier)).await;
                let message_id = authentication::session::tests::message(&database, services.authentication(), &mut supplier, "13700000000").await;
                let authentication::session::Receipt::Registration(credential) = Box::pin(services.authentication().create_session(authentication::session::tests::input(Uuid::new_v4(), message_id, "13700000000", authentication::session::tests::CODE))).await.expect("peer R1") else { panic!("registration credential"); };
                let registered = services.registration().register(crate::registration::Input::new(operation(), credential.id(), credential.token().text().parse().expect("proof"), "Other Member".parse().expect("name"), "000000200002290088".parse().expect("id number"))).await.expect("peer R2");
                let peer = services.current().authenticate(current::Proof { id: registered.id(), token: registered.token().text().parse().expect("token") }).await.expect("peer actor");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13700000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("peer confirmation");
                let (peer_grant, _) = Box::pin(services.grant().create(institution.clone(), operation(), admission(&candidate, "institution", "13700000000", 1))).await.expect("peer administrator");
                let mut body = json!({"target":{"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13700000000"}});
                assert!(matches!(services.grant().termination_candidate(&manager, "13700000000".parse().expect("mobile")).await, Err(Error::Absent)));
                body["target"]["revisionId"] = json!(Uuid::new_v4());
                assert!(matches!(Box::pin(services.grant().terminate_member(manager.clone(), operation(), peer_grant.id(), serde_json::from_value(body).expect("stale peer input"))).await, Err(Error::Absent)), "peer is concealed before stale confirmation");
                let operator = services.current().read(&manager).await.expect("current operator revision");
                let writer = database.connections(&database.user).recover().await.expect("runtime writer");
                grant::tests::sql_transition(&database, &services, peer_grant.id(), operator.revision_id(), grant::tests::SqlAction::ManagerEnd).await;
                Box::pin(services.grant().terminate(peer.clone(), operation(), peer_grant.id())).await.expect("peer can exit without manager authority");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13700000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("ordinary rejoin candidate");
                let (grant, _) = Box::pin(services.grant().create(institution.clone(), operation(), admission(&candidate, "institution", "13700000000", 0))).await.expect("ordinary new grant");
                let candidate = services.grant().termination_candidate(&manager, "13700000000".parse().expect("mobile")).await.expect("manageable target");
                grant::tests::sql_transition(&database, &services, grant.id(), operator.revision_id(), grant::tests::SqlAction::ManagerEnd).await;
                let paused = Box::pin(services.suspension().suspend(institution, operation(), candidate.identity().id(), serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13700000000"})).expect("pause confirmation"))).await.expect("account pause retains domain membership");
                assert!(matches!(services.current().read(&peer).await, Err(current::Error::Invalid)));
                services.grant().termination_candidate(&manager, "13700000000".parse().expect("mobile")).await.expect("suspended account remains manageable");
                let ended = Box::pin(services.grant().terminate_member(manager, operation(), grant.id(), serde_json::from_value(confirmation(&candidate, "13700000000")).expect("confirmation before pause"))).await.expect("manager ends suspended target");
                assert_eq!(ended.sequence(), 2);
                assert!(writer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.suspensions WHERE id=$1 AND sequence=2) AND NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id WHERE account.id=$2 AND account.suspension_id=$1 AND account.sequence=2 AND person.sequence=1) AND EXISTS(SELECT 1 FROM access.grants WHERE id=$3 AND termination='manager' AND sequence=2) AND (SELECT count(*) FROM access.terminations)=2", &[&paused.id(),&candidate.identity().id(),&grant.id()]).await.expect("termination preserves the Account pause and advances only its Grant").get::<_,bool>(0));
                writer.terminate().await.expect("writer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.close(); services.drain().await.expect("restart validates both modes"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("managed database boundary deadline");
        }));
    }

    fn confirmation(candidate: &member::Candidate, mobile: &str) -> serde_json::Value {
        let subject = candidate.identity();
        json!({"target":{"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":mobile}})
    }

    async fn managed_member(
        database: &Database,
        services: &crate::startup::Services,
        supplier: &mut Supplier,
    ) -> (current::Actor, current::Actor, member::Candidate) {
        let institution = actor(database, services, supplier).await;
        let manager = register(database, services, supplier).await;
        let candidate = services
            .grant()
            .candidate(
                &institution,
                Entry::Institution,
                "13800000000".parse().expect("mobile"),
                crate::pagination::Pagination::new(1, 20).expect("pagination"),
            )
            .await
            .expect("domain manager candidate");
        Box::pin(
            services
                .grant()
                .create(institution.clone(), operation(), input(&candidate, 1)),
        )
        .await
        .expect("institution admits a domain administrator");
        let candidate = services
            .grant()
            .candidate(
                &manager,
                Entry::Domain,
                "13900000000".parse().expect("mobile"),
                crate::pagination::Pagination::new(1, 20).expect("pagination"),
            )
            .await
            .expect("higher institution rank is not used by the domain");
        let admission = json!({"entry":"domain","target":{"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13900000000","assignmentId":candidate.assignment_id(),"suspensionId":candidate.suspension_id(),"grant":candidate.grant()},"role":{"id":candidate.roles()[0].id(),"changeId":candidate.roles()[0].change_id()}});
        Box::pin(services.grant().create(
            manager.clone(),
            operation(),
            serde_json::from_value(admission).expect("typed admission"),
        ))
        .await
        .expect("ordinary domain member");
        let candidate = services
            .grant()
            .termination_candidate(&manager, "13900000000".parse().expect("mobile"))
            .await
            .expect("managed candidate");
        (manager, institution, candidate)
    }

    #[test]
    fn managed_termination_binds_domain_authority_and_preserves_the_original_after_rejoining() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(50), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (manager, holder, candidate) = Box::pin(managed_member(&database, &services, &mut supplier)).await;
                let body = confirmation(&candidate, "13900000000");
                let id = candidate.grant_id();
                assert!(matches!(services.grant().termination_candidate(&holder, "13800000000".parse().expect("mobile")).await, Err(Error::Denied)), "institution administrator has no domain termination authority");
                for mobile in ["13800000000", "13700000000"] {
                    assert!(matches!(services.grant().termination_candidate(&manager, mobile.parse().expect("mobile")).await, Err(Error::Absent)));
                }
                assert!(matches!(Box::pin(services.grant().terminate_member(holder.clone(), operation(), id, serde_json::from_value(body.clone()).expect("confirmation"))).await, Err(Error::Denied)));
                for field in ["mobile", "personId", "revisionId"] {
                    let mut changed = body.clone();
                    changed["target"][field] = if field == "mobile" { json!("13700000000") } else { json!(Uuid::new_v4()) };
                    assert!(matches!(Box::pin(services.grant().terminate_member(manager.clone(), operation(), id, serde_json::from_value(changed).expect("typed stale confirmation"))).await, Err(Error::TargetChanged)), "{field}");
                }
                let unknown: Id = serde_json::from_value(json!(Uuid::new_v4())).expect("other grant");
                assert!(matches!(Box::pin(services.grant().terminate_member(manager.clone(), operation(), unknown, serde_json::from_value(body.clone()).expect("confirmation"))).await, Err(Error::TargetChanged)));
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let unchanged = "SELECT jsonb_build_object('accounts',(SELECT jsonb_agg(to_jsonb(account) ORDER BY id) FROM identity.accounts account),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session),'holders',(SELECT jsonb_agg(to_jsonb(holder) ORDER BY id) FROM access.holders holder))::text";
                let before: String = observer.client().query_one(unchanged, &[]).await.expect("account facts before termination").get(0);
                let original = operation();
                let result = Box::pin(services.grant().terminate_member(manager.clone(), original, id, serde_json::from_value(body.clone()).expect("confirmation"))).await.expect("domain administrator ends higher institutional account's participation");
                assert_eq!(result.mode(), Mode::Manager);
                assert_eq!(result.subject().person_id(), candidate.identity().person_id());
                assert_eq!(result.operator().name(), "T*");
                assert_ne!(result.operator().person_id(), result.subject().person_id());
                assert_eq!(result.sequence(), 2);
                let after: String = observer.client().query_one(unchanged, &[]).await.expect("account facts after termination").get(0);
                assert_eq!(before, after, "ending participation preserves accounts, sessions and institution assignments");
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.terminations)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=1 AND EXISTS(SELECT 1 FROM access.terminations termination JOIN access.grants state ON state.id=termination.id JOIN identity.persons person ON person.id=termination.person_id WHERE termination.id=$1 AND termination.operation_id=$2 AND termination.operator_id=$3 AND termination.revision_id=$4 AND termination.sequence=2 AND state.sequence=2 AND state.termination='manager' AND person.sequence=1 AND octet_length(termination.seal)=32 AND termination.seal_key_id=0) AND EXISTS(SELECT 1 FROM access.completions WHERE family='grant_termination' AND operation_id=$2)", &[&id,&original,&result.operator().revision_id(),&candidate.identity().revision_id()]).await.expect("one proven complete Grant transition").get::<_,bool>(0));
                for field in ["mobile", "accountId", "personId", "revisionId"] {
                    let mut changed = body.clone();
                    changed["target"][field] = if field == "mobile" { json!("13700000000") } else { json!(Uuid::new_v4()) };
                    assert!(matches!(Box::pin(services.grant().terminate_member(manager.clone(), original, id, serde_json::from_value(changed).expect("other typed input"))).await, Err(Error::Mismatch)), "original {field}");
                }
                assert!(matches!(Box::pin(services.grant().terminate(holder.clone(), original, id)).await, Err(Error::Mismatch)), "self mode cannot claim the manager's original");
                assert!(matches!(Box::pin(services.grant().terminate_member(manager.clone(), operation(), id, serde_json::from_value(body.clone()).expect("confirmation"))).await, Err(Error::Absent)), "new management requests do not disclose ended membership");
                services.current().read(&holder).await.expect("target session remains valid");
                let replacement = services.grant().candidate(&manager, Entry::Domain, "13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("rejoin candidate");
                let admission = json!({"entry":"domain","target":{"accountId":replacement.identity().id(),"personId":replacement.identity().person_id(),"revisionId":replacement.identity().revision_id(),"mobile":"13900000000","assignmentId":replacement.assignment_id(),"suspensionId":replacement.suspension_id(),"grant":replacement.grant()},"role":{"id":replacement.roles()[0].id(),"changeId":replacement.roles()[0].change_id()}});
                let (replacement, _) = Box::pin(services.grant().create(manager.clone(), operation(), serde_json::from_value(admission).expect("rejoin input"))).await.expect("new grant");
                assert!(matches!(Box::pin(services.grant().terminate_member(manager.clone(), operation(), id, serde_json::from_value(body.clone()).expect("old grant confirmation"))).await, Err(Error::TargetChanged)));
                let correction = services.correction().candidate_by_mobile(&holder, "13800000000".parse().expect("manager mobile")).await.expect("manager identity confirmation");
                Box::pin(services.correction().correct(holder.clone(), operation(), serde_json::from_value(json!({"target":correction.target(),"name":"Changed Manager","idNumber":"00000020000229007X"})).expect("correction"))).await.expect("later operator correction");
                let replay = Box::pin(services.grant().terminate_member(manager.clone(), original, id, serde_json::from_value(body.clone()).expect("original input"))).await.expect("replay keeps original operator revision");
                assert_eq!(replay.terminated_at(), result.terminated_at());
                assert_eq!(replay.operator().revision_id(), result.operator().revision_id());
                assert_eq!(replay.operator().name(), "T*");
                assert_eq!(services.grant().read_latest(&holder).await.expect("new grant remains").id(), replacement.id());
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.terminations)=1 AND EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND termination IS NULL AND sequence=1) AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$2 AND sequence=1)", &[&replacement.id(),&candidate.identity().person_id()]).await.expect("late replay retains replacement and sequence").get::<_,bool>(0));
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let replay = Box::pin(services.grant().terminate_member(manager.clone(), original, id, serde_json::from_value(body).expect("original input"))).await.expect("restart retains original proof");
                assert_eq!(replay.operator().revision_id(), result.operator().revision_id());
                let current = services.current().read(&manager).await.expect("current manager identity");
                Box::pin(services.cancellation().cancel_account(holder.clone(), operation(), manager.account_id(), serde_json::from_value(json!({"personId":current.person_id(),"revisionId":current.revision_id(),"mobile":"13800000000"})).expect("operator cancellation confirmation"))).await.expect("later operator cancellation");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                assert_eq!(services.grant().read_latest(&holder).await.expect("historical managed end survives its operator's cancellation").id(), replacement.id());
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("managed lifecycle deadline");
        }));
    }

    #[test]
    fn managed_termination_recovers_proven_input_and_retains_accepted_work() {
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
                    let (manager, holder, candidate) = Box::pin(managed_member(&database, &services, &mut supplier)).await;
                    let body = confirmation(&candidate, "13900000000");
                    let id = candidate.grant_id();
                    let operation_id = operation();
                    if case == "revoked" {
                        let original = crate::recovery::after_revocation(&database, &proxy, services.revocation(), &manager,
                            Box::pin(services.grant().terminate_member(manager.clone(), operation_id, id, serde_json::from_value(body.clone()).expect("confirmation"))))
                            .await.expect("later revocation cannot discard this work's original result");
                        assert_eq!(original.id(), id);
                        assert_eq!(original.mode(), Mode::Manager);
                        assert!(matches!(Box::pin(services.grant().terminate_member(manager.clone(), operation_id, id, serde_json::from_value(body.clone()).expect("confirmation"))).await, Err(Error::Current(current::Error::Invalid))));
                        services.close(); services.drain().await.expect("drained");
                    } else {
                        proxy.arm(match case { "lost" => Fault::LostCommit, "rollback" => Fault::RolledBack, "unknown" => Fault::UnknownCommit, "cleanup" | "cancelled" => Fault::DelayedCommit, _ => unreachable!() });
                        let service = services.grant().clone(); let actor = manager.clone(); let input = serde_json::from_value(body.clone()).expect("confirmation");
                        let waiter = tokio::spawn(async move { service.terminate_member(actor, operation_id, id, input).await });
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original commit intercepted");
                        if case == "cleanup" { proxy.arm(Fault::ReadCommitTermination); }
                        if case == "cancelled" {
                            waiter.abort(); assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                            services.close();
                            assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "cancelled wait retains accepted work until original completion");
                            proxy.release(); services.drain().await.expect("accepted work drained");
                        } else {
                            proxy.release();
                            let result = timeout(Duration::from_secs(15), waiter).await.expect("bounded outcome").expect("waiter");
                            match case { "rollback" => assert!(matches!(result, Err(Error::Command(crate::kernel::Error::Unavailable(ref fault))) if fault.stage()==crate::kernel::Stage::Commit)), "unknown" => assert!(matches!(result, Err(Error::Command(crate::kernel::Error::Unknown(_))))), _ => assert_eq!(result.expect("permanent managed result").mode(), Mode::Manager) }
                            services.close(); services.drain().await.expect("drained");
                        }
                    }
                    context.stop().await.expect("stopped");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT id FROM access.grants WHERE id=$1 FOR UPDATE", &[&id]).await.expect("original backend released its target lock");
                    let committed = i64::from(case != "rollback");
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.grants),(SELECT count(*) FROM access.assignments),(SELECT count(*) FROM access.terminations),(SELECT count(*) FROM access.grants WHERE termination IS NULL),sequence FROM access.grants WHERE id=$1", &[&id]).await.expect("independent durable state before replay");
                    assert_eq!(row.get::<_,i64>(0),2); assert_eq!(row.get::<_,i64>(1),2); assert_eq!(row.get::<_,i64>(2),committed); assert_eq!(row.get::<_,i64>(3),2-committed); assert_eq!(row.get::<_,i64>(4),1+committed);
                    observer.client().batch_execute("COMMIT").await.expect("observation complete");
                    proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    if case != "revoked" {
                        let original = Box::pin(services.grant().terminate_member(manager, operation_id, id, serde_json::from_value(body).expect("original confirmation"))).await.expect("original result or explicit retry after proven rollback");
                        assert_eq!(original.sequence(),2);
                    }
                    assert_eq!(services.grant().read_latest(&holder).await.expect("retained terminated grant").status(),Status::Terminated);
                    assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.terminations)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=1 AND EXISTS(SELECT 1 FROM access.terminations termination JOIN access.grants state ON state.id=termination.id JOIN identity.persons person ON person.id=termination.person_id WHERE termination.operation_id=$1 AND termination.sequence=2 AND state.sequence=2 AND state.termination='manager' AND person.sequence=1 AND octet_length(termination.seal)=32 AND termination.seal_key_id=0) AND EXISTS(SELECT 1 FROM access.completions WHERE family='grant_termination' AND operation_id=$1)", &[&operation_id]).await.expect("one complete proven original after restart and replay").get::<_,bool>(0));
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("managed recovery deadline");
            }
        }));
    }

    #[test]
    fn managed_termination_checks_its_own_retained_input_key() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (manager, holder, candidate) = Box::pin(managed_member(&database, &services, &mut supplier)).await;
                let operation_id = operation();
                let body = confirmation(&candidate,"13900000000");
                Box::pin(services.grant().terminate_member(manager.clone(),operation_id,candidate.grant_id(),serde_json::from_value(body.clone()).expect("confirmation"))).await.expect("managed end");
                let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; INSERT INTO security.keys(id,fingerprint,active) SELECT 1,fingerprint,false FROM security.keys WHERE id=0; UPDATE access.completions SET seal_key_id=1 WHERE family='grant_termination'; COMMIT").await.expect("only termination completion refers to unavailable old material");
                assert!(owner.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM access.grants WHERE seal_key_id<>0) AND EXISTS(SELECT 1 FROM access.completions WHERE family='grant_termination' AND seal_key_id=1)",&[]).await.expect("distinct retained input reference").get::<_,bool>(0));
                assert!(services.grant().verify(owner.client()).await.is_err(),"startup consumer must scan termination references as well as grants");
                let error = Box::pin(services.grant().terminate_member(manager,operation_id,candidate.grant_id(),serde_json::from_value(body).expect("original confirmation"))).await.err().expect("missing proof key is not a mismatch or success");
                assert_eq!(error.failure(),crate::access::grant::Failure::Unavailable);
                assert!(services.ready().await.is_err(),"missing permanent material closes acceptance");
                assert!(owner.client().query_one("SELECT (SELECT count(*) FROM access.terminations)=1 AND EXISTS(SELECT 1 FROM identity.accounts WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$2 AND sequence=1) AND EXISTS(SELECT 1 FROM access.grants WHERE person_id=$2 AND sequence=2 AND termination='manager')",&[&holder.account_id(),&candidate.identity().person_id()]).await.expect("failure does not repeat termination").get::<_,bool>(0));
                owner.terminate().await.expect("owner ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("retained termination key deadline");
        }));
    }

    #[test]
    fn managed_termination_rechecks_both_participations_after_waiting() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for ending_manager in [false,true] {
                timeout(Duration::from_secs(40), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (manager, holder, candidate) = Box::pin(managed_member(&database, &services, &mut supplier)).await;
                    let source = services.grant().read_latest(&manager).await.expect("manager grant");
                    let id = if ending_manager { source.id() } else { candidate.grant_id() };
                    let blocker = database.connections("postgres").recover().await.expect("disposable grant blocker");
                    blocker.client().batch_execute("BEGIN").await.expect("block original grant");
                    blocker.client().query_one("SELECT id FROM access.grants WHERE id=$1 FOR UPDATE", &[&id]).await.expect("exclusive grant lock");
                    let pid: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("blocker PID").get(0);
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    let service = services.grant().clone(); let actor = if ending_manager { manager.clone() } else { holder.clone() }; let first = operation();
                    let exit = tokio::spawn(async move { service.terminate(actor, first, id).await });
                    wait_for_lock(observer.client(),pid,"SELECT id FROM access.grants WHERE id=$1 FOR UPDATE").await;
                    let service = services.grant().clone(); let target = candidate.grant_id(); let input = serde_json::from_value(confirmation(&candidate,"13900000000")).expect("managed confirmation");
                    let managed = tokio::spawn(async move { service.terminate_member(manager, operation(), target, input).await });
                    wait_for_lock(observer.client(),pid,if ending_manager { "SELECT id FROM access.grants WHERE id=$1 FOR SHARE" } else { "SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE" }).await;
                    blocker.client().batch_execute("COMMIT").await.expect("release original grant");
                    timeout(Duration::from_secs(5),exit).await.expect("self exit deadline").expect("exit task").expect("first exit commits");
                    let error = timeout(Duration::from_secs(5),managed).await.expect("management deadline").expect("managed task").err().expect("stale actor or member rejected");
                    if ending_manager { assert!(matches!(error,Error::Denied),"{error:?}"); } else { assert!(matches!(error,Error::Absent),"{error:?}"); }
                    assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.terminations)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=1 AND EXISTS(SELECT 1 FROM access.terminations WHERE operation_id=$1 AND mode='self' AND sequence=2) AND EXISTS(SELECT 1 FROM access.completions WHERE family='grant_termination' AND operation_id=$1 AND action='leave')",&[&first]).await.expect("only the earlier self exit committed").get::<_,bool>(0));
                    services.current().read(&holder).await.expect("domain transition preserves target session");
                    observer.terminate().await.expect("observer ended"); blocker.terminate().await.expect("blocker ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("managed lock coordination deadline");
            }
        }));
    }

    #[test]
    fn self_termination_preserves_identity_and_replays_after_rejoining_and_cancellation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let holder = register(&database, &services, &mut supplier).await;
                let original_candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                let admission = operation();
                let (grant, _) = Box::pin(services.grant().create(manager.clone(), admission, input(&original_candidate, 0))).await.expect("ordinary member admitted");
                let unknown: Id = serde_json::from_value(json!(Uuid::new_v4())).expect("grant id");
                assert!(matches!(Box::pin(services.grant().terminate(holder.clone(), operation(), unknown)).await, Err(Error::Absent)));
                assert!(matches!(Box::pin(services.grant().terminate(manager.clone(), operation(), grant.id())).await, Err(Error::Absent)), "self exit cannot select another person's grant");
                let observer = database.connections(&database.user).recover().await.expect("runtime observer");
                let snapshot = "SELECT jsonb_build_object('accounts',(SELECT jsonb_agg(to_jsonb(account) ORDER BY id) FROM identity.accounts account),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session),'holders',(SELECT jsonb_agg(to_jsonb(holder) ORDER BY id) FROM access.holders holder))::text";
                let before: String = observer.client().query_one(snapshot, &[]).await.expect("account-owned facts before exit").get(0);
                let operation_id = operation();
                let termination = Box::pin(services.grant().terminate(holder.clone(), operation_id, grant.id())).await.expect("holder exits without manager permission");
                assert_eq!(termination.id(), grant.id());
                assert_eq!(termination.operation_id(), operation_id);
                assert_eq!(termination.sequence(), 2);
                assert_eq!(termination.mode(), Mode::Current);
                assert_eq!(termination.subject().revision_id(), original_candidate.identity().revision_id());
                assert_eq!(termination.operator().revision_id(), termination.subject().revision_id());
                assert_eq!(termination.subject().name(), "T*");
                let after: String = observer.client().query_one(snapshot, &[]).await.expect("account-owned facts after exit").get(0);
                assert_eq!(before, after, "exit preserves accounts, sessions and institution roles byte for byte");
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.grants)=1 AND (SELECT count(*) FROM access.assignments)=1 AND (SELECT count(*) FROM access.terminations)=1 AND NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL) AND EXISTS(SELECT 1 FROM access.terminations termination JOIN access.grants state ON state.id=termination.id JOIN identity.persons person ON person.id=termination.person_id WHERE termination.id=$1 AND termination.operation_id=$2 AND termination.sequence=2 AND state.sequence=2 AND state.termination='self' AND person.sequence=1 AND termination.operator_id=termination.revision_id)", &[&grant.id(), &operation_id]).await.expect("one paired permanent end and Grant sequence").get::<_,bool>(0));
                services.current().read(&holder).await.expect("same session remains usable");
                assert!(matches!(services.grant().read_role(&holder).await, Err(Error::RoleAbsent)));
                assert_eq!(services.grant().read_latest(&holder).await.expect("holder reads ended grant").status(), Status::Terminated);
                assert!(matches!(Box::pin(services.grant().terminate(holder.clone(), operation(), grant.id())).await, Err(Error::StateConflict)));
                assert!(matches!(Box::pin(services.grant().terminate(holder.clone(), operation_id, unknown)).await, Err(Error::Mismatch)));
                let correction = services.correction().candidate_by_mobile(&manager, "13800000000".parse().expect("mobile")).await.expect("correction confirmation");
                Box::pin(services.correction().correct(manager.clone(), operation(), serde_json::from_value(json!({"target":correction.target(),"name":"Changed Holder","idNumber":"00000020000229007X"})).expect("correction input"))).await.expect("later real-name correction");
                let replayed = Box::pin(services.grant().terminate(holder.clone(), operation_id, grant.id())).await.expect("replay retains original identity");
                assert_eq!(replayed.subject().name(), "T*");
                assert_eq!(replayed.subject().revision_id(), termination.subject().revision_id());
                assert_eq!(replayed.terminated_at(), termination.terminated_at());
                let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("ended grant can rejoin");
                let (replacement, _) = Box::pin(services.grant().create(manager.clone(), operation(), input(&candidate, 0))).await.expect("new participation");
                assert_ne!(replacement.id(), grant.id());
                assert_eq!(replacement.sequence(), 1);
                Box::pin(services.grant().terminate(holder.clone(), operation_id, grant.id())).await.expect("late old exit returns only the old result");
                assert!(matches!(Box::pin(services.grant().terminate(holder.clone(), operation_id, replacement.id())).await, Err(Error::Mismatch)));
                assert_eq!(services.grant().read_latest(&holder).await.expect("new grant survives replay").id(), replacement.id());
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.terminations)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=1 AND EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND termination IS NULL AND sequence=1) AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$2 AND sequence=2)", &[&replacement.id(), &candidate.identity().person_id()]).await.expect("replay does not remove new membership or increment history").get::<_,bool>(0));
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                Box::pin(services.grant().terminate(holder.clone(), operation_id, grant.id())).await.expect("permanent result survives restart");
                Box::pin(services.cancellation().cancel_account(manager.clone(), operation(), candidate.identity().id(), serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("later cancellation ends only the remaining membership");
                assert!(matches!(Box::pin(services.grant().terminate(holder.clone(), operation_id, grant.id())).await, Err(Error::Current(current::Error::Invalid))));
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.terminations)=2 AND (SELECT count(*) FROM access.grants)=2 AND NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL) AND (SELECT count(*) FROM access.terminations WHERE person_id=$1 AND mode='self')=1 AND (SELECT count(*) FROM access.terminations WHERE person_id=$1 AND mode='cancellation')=1 AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$1 AND sequence=3) AND EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$1 AND sequence=3)", &[&candidate.identity().person_id()]).await.expect("distinct original end sources after cancellation").get::<_,bool>(0));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.current().read(&manager).await.expect("restart accepts both retained end sources");
                let (replayed, _) = Box::pin(services.grant().create(manager, admission, input(&original_candidate, 0))).await.expect("original admission retains its manually ended grant after cancellation");
                assert_eq!(replayed.id(), grant.id());
                assert_eq!(replayed.status(), Status::Terminated);
                assert_eq!(replayed.sequence(), 1);
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
            })).await.expect("self termination lifecycle deadline");
        }));
    }

    #[test]
    fn self_termination_sql_requires_one_complete_grant_transition() {
        use tokio_postgres::error::SqlState;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(40),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let holder = register(&database, &services, &mut supplier).await;
                    let candidate = services
                        .grant()
                        .candidate(
                            &manager,
                            Entry::Institution,
                            "13800000000".parse().expect("mobile"),
                            crate::pagination::Pagination::new(1, 20).expect("pagination"),
                        )
                        .await
                        .expect("candidate");
                    let (grant, _) = Box::pin(services.grant().create(
                        manager,
                        operation(),
                        input(&candidate, 0),
                    ))
                    .await
                    .expect("member admitted");
                    let writer = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("runtime writer");
                    grant::tests::sql_transition(
                        &database,
                        &services,
                        grant.id(),
                        grant.subject().revision_id(),
                        grant::tests::SqlAction::SelfEnd,
                    )
                    .await;
                    Box::pin(services.grant().terminate(holder, operation(), grant.id()))
                        .await
                        .expect("valid complete transition");
                    for sql in [
                        "UPDATE access.terminations SET terminated_at=terminated_at",
                        "DELETE FROM access.terminations",
                    ] {
                        assert_eq!(
                            writer
                                .client()
                                .execute(sql, &[])
                                .await
                                .expect_err("runtime cannot rewrite history")
                                .code(),
                            Some(&SqlState::INSUFFICIENT_PRIVILEGE)
                        );
                    }
                    let user = platform::database::User::new(database.user.clone())
                        .expect("runtime identity");
                    crate::database::permissions::verify(writer.client(), &user)
                        .await
                        .expect("declared runtime permissions");
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
            .expect("termination SQL deadline");
        }));
    }

    #[test]
    fn a_new_grant_uses_its_state_while_old_ends_remain_authenticated() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["missing", "future", "sequence", "revoked"] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let holder = register(&database, &services, &mut supplier).await;
                    let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                    let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation(), input(&candidate, 0))).await.expect("first grant");
                    let original_exit = operation();
                    Box::pin(services.grant().terminate(holder.clone(), original_exit, grant.id())).await.expect("first exit");
                    let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("new candidate");
                    let (replacement, _) = Box::pin(services.grant().create(manager, operation(), input(&candidate, 0))).await.expect("rejoined member");
                    if case == "revoked" { Box::pin(services.revocation().revoke(holder.clone(), operation(), crate::identity::revocation::Scope::Current)).await.expect("real session revocation"); }
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated historical damage");
                    let sql = match case {
                        "missing" | "revoked" => "DELETE FROM access.terminations WHERE id=$1",
                        "future" => "UPDATE access.terminations SET terminated_at=clock_timestamp()+interval '1 day' WHERE id=$1",
                        "sequence" => "UPDATE access.terminations SET sequence=sequence+1000 WHERE id=$1",
                        _ => unreachable!(),
                    };
                    assert_eq!(owner.client().execute(sql, &[&grant.id()]).await.expect("damage only the old end"),1);
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let result = Box::pin(services.grant().terminate(holder.clone(), operation(), replacement.id())).await;
                    if case == "revoked" {
                        assert!(matches!(result, Err(Error::Current(current::Error::Invalid))));
                        services.ready().await.expect("invalid identity prevents reading history");
                    } else {
                        let ended = result.expect("new work uses the new Grant's authenticated state");
                        assert_eq!(ended.id(), replacement.id());
                        assert_eq!(ended.sequence(), 2);
                        services.ready().await.expect("unconsumed old history does not close admission");
                        let error = Box::pin(services.grant().terminate(holder, original_exit, grant.id())).await.err().expect("taking the old result rejects its damaged original record");
                        assert!(crate::runtime::work::Failure::unreliable(&error), "{case}: {error:?}");
                        assert!(services.ready().await.is_err(), "consumed record damage closes acceptance");
                    }
                    assert!(services.grant().verify(owner.client()).await.is_err(), "startup rejects the same historical damage: {case}");
                    let ended = case!="revoked";
                    assert!(owner.client().query_one("SELECT EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND (termination IS NOT NULL)=$3 AND sequence=CASE WHEN $3 THEN 2 ELSE 1 END) AND EXISTS(SELECT 1 FROM access.terminations WHERE id=$1)=$3 AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$2 AND sequence=1)", &[&replacement.id(),&candidate.identity().person_id(),&ended]).await.expect("only the accepted new exit changes its own Grant").get::<_,bool>(0));
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("retained termination damage deadline");
            }
        }));
    }

    #[test]
    fn self_termination_rechecks_account_suspension_after_waiting() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let holder = register(&database, &services, &mut supplier).await;
                let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation(), input(&candidate, 0))).await.expect("member");
                let blocker = database.connections("postgres").recover().await.expect("isolated account blocker");
                blocker.client().batch_execute("BEGIN").await.expect("block account");
                blocker.client().query_one("SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE", &[&holder.account_id()]).await.expect("exclusive account lock");
                let pid: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("blocker PID").get(0);
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let service = services.suspension().clone(); let account_id = holder.account_id();
                let confirmation = serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"})).expect("pause confirmation");
                let pause = tokio::spawn(async move { service.suspend(manager, operation(), account_id, confirmation).await });
                wait_for_lock(observer.client(), pid, "SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE").await;
                let service = services.grant().clone(); let id = grant.id();
                let exit = tokio::spawn(async move { service.terminate(holder, operation(), id).await });
                wait_for_lock(observer.client(), pid, "SELECT id FROM identity.accounts WHERE id=$1 FOR SHARE").await;
                blocker.client().batch_execute("COMMIT").await.expect("release account");
                let paused=timeout(Duration::from_secs(3), pause).await.expect("pause deadline").expect("pause task").expect("account paused");
                assert!(matches!(timeout(Duration::from_secs(3), exit).await.expect("exit deadline").expect("exit task"), Err(Error::Current(current::Error::Invalid))));
                let facts=observer.client().query_one(
                    "SELECT NOT EXISTS(SELECT 1 FROM access.terminations)
                            AND (SELECT count(*) FROM access.grants)=1
                            AND (SELECT count(*) FROM access.assignments)=1
                            AND EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND termination IS NULL) AS participation,
                        EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND person_id=$2 AND sequence=1)
                            AND EXISTS(SELECT 1 FROM access.assignments WHERE grant_id=$1 AND person_id=$2 AND sequence=1)
                            AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$2 AND sequence=1) AS personal,
                        EXISTS(SELECT 1 FROM identity.accounts account
                            JOIN identity.creations creation ON creation.id=account.id AND creation.person_id=account.person_id
                            JOIN identity.suspensions suspension ON suspension.id=account.suspension_id AND suspension.account_id=account.id AND suspension.person_id=account.person_id
                            WHERE account.id=$3 AND account.person_id=$2 AND account.cancellation_id IS NULL
                                AND creation.sequence=1 AND account.created_at=creation.created_at
                                AND account.sequence=2 AND account.version=2 AND suspension.id=$4 AND suspension.sequence=2
                                AND account.changed_at=suspension.suspended_at)
                            AND (SELECT count(*) FROM identity.suspensions WHERE account_id=$3)=1
                            AND NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE account_id=$3) AS account",
                    &[&id,&candidate.identity().person_id(),&candidate.identity().id(),&paused.id()],
                ).await.expect("rejected exit preserves Grant sequence and the independent Account pause");
                for column in ["participation","personal","account"] {assert!(facts.get::<_,bool>(column),"{column}");}
                services.ready().await.expect("account rejection is not damaged storage");
                observer.terminate().await.expect("observer ended"); blocker.terminate().await.expect("blocker ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("queued account pause deadline");
        }));
    }

    async fn wait_for_lock(client: &tokio_postgres::Client, blocker: i32, query: &str) {
        let result = timeout(Duration::from_secs(3), async { loop {
            if client.query_one("WITH RECURSIVE blocked(pid) AS (SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) UNION SELECT backend.pid FROM pg_stat_activity backend JOIN blocked ON blocked.pid=ANY(pg_blocking_pids(backend.pid)) WHERE backend.datname=current_database()) SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid IN (SELECT pid FROM blocked) AND wait_event_type='Lock' AND query LIKE $2)", &[&blocker,&query]).await.expect("actual wait graph").get::<_,bool>(0) { break; }
            tokio::task::yield_now().await;
        }}).await;
        if result.is_err() {
            let rows = client.query("SELECT query, wait_event_type, pg_blocking_pids(pid)::text FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid()", &[]).await.expect("wait failure diagnostics");
            let states: Vec<_> = rows
                .iter()
                .map(|row| {
                    (
                        row.get::<_, String>(0),
                        row.get::<_, Option<String>>(1),
                        row.get::<_, String>(2),
                    )
                })
                .collect();
            panic!("operation did not reach {query}: {states:?}");
        }
    }

    #[test]
    fn self_termination_coordinates_domain_authority_and_duplicate_exits() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for domain in [false, true] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let holder = register(&database, &services, &mut supplier).await;
                    let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                    let (source, _) = Box::pin(services.grant().create(institution, operation(), input(&candidate, 1))).await.expect("domain manager admitted");
                    let target = services.grant().candidate(&holder, Entry::Domain, "13900000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("domain admission confirmation");
                    let admission: Input = serde_json::from_value(json!({"entry":"domain","target":{
                        "accountId":target.identity().id(),"personId":target.identity().person_id(),"revisionId":target.identity().revision_id(),
                        "mobile":"13900000000","assignmentId":target.assignment_id(),"suspensionId":target.suspension_id(),"grant":target.grant()
                    },"role":{"id":target.roles()[0].id(),"changeId":target.roles()[0].change_id()}})).expect("domain input");
                    let blocker = database.connections("postgres").recover().await.expect("isolated grant lock holder");
                    blocker.client().batch_execute("BEGIN").await.expect("block source grant");
                    blocker.client().query_one("SELECT id FROM access.grants WHERE id=$1 FOR UPDATE", &[&source.id()]).await.expect("source lock");
                    let pid: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("blocker PID").get(0);
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let service = services.grant().clone(); let actor = holder.clone(); let id = source.id(); let first = operation();
                    let waiter = tokio::spawn(async move { service.terminate(actor, first, id).await });
                    wait_for_lock(observer.client(), pid, "SELECT id FROM access.grants WHERE id=$1 FOR UPDATE").await;
                    let service = services.grant().clone(); let actor = holder.clone();
                    let competitor = tokio::spawn(async move {
                        if domain { service.create(actor, operation(), admission).await.map(|_| ()) }
                        else { service.terminate(actor, operation(), id).await.map(|_| ()) }
                    });
                    wait_for_lock(observer.client(), pid, if domain { "SELECT id FROM access.grants WHERE person_id=$1%FOR SHARE" } else { "SELECT id FROM access.grants WHERE id=$1 FOR UPDATE" }).await;
                    waiter.abort(); assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                    services.close();
                    assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "accepted exit still owns its work and locks");
                    blocker.client().batch_execute("COMMIT").await.expect("release grant");
                    let error = timeout(Duration::from_secs(3), competitor).await.expect("competitor completed").expect("competitor task").expect_err("current authorization changed");
                    if domain { assert!(matches!(error, Error::Denied), "{error:?}"); }
                    else { assert!(matches!(error, Error::StateConflict), "{error:?}"); }
                    services.drain().await.expect("accepted operations drained");
                    assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.grants)=1 AND (SELECT count(*) FROM access.assignments)=1 AND (SELECT count(*) FROM access.terminations)=1 AND NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL) AND EXISTS(SELECT 1 FROM access.terminations WHERE operation_id=$1 AND mode='self' AND sequence=2) AND EXISTS(SELECT 1 FROM access.grants WHERE person_id=$2 AND sequence=2 AND termination='self') AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$2 AND sequence=1) AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$3 AND sequence=1)", &[&first,&source.subject().person_id(),&source.operator().person_id()]).await.expect("only accepted exit changed permanent facts").get::<_,bool>(0));
                    observer.terminate().await.expect("observer ended"); blocker.terminate().await.expect("blocker ended"); context.stop().await.expect("stopped");
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    services.current().read(&holder).await.expect("loss of domain authority does not revoke the session");
                    Box::pin(services.grant().terminate(holder, first, id)).await.expect("cancelled caller can replay original exit");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("termination coordination deadline");
            }
        }));
    }

    #[test]
    fn committed_termination_recovers_after_the_account_loses_access() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for cancelled in [false, true] {
                timeout(Duration::from_secs(40), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let holder = register(&database, &services, &mut supplier).await;
                    let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                    let (grant, _) = Box::pin(services.grant().create(manager.clone(), operation(), input(&candidate, 0))).await.expect("member admitted");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let unrelated = database.connections(&database.user).recover().await.expect("unrelated runtime connection");
                    unrelated.client().batch_execute("BEGIN; SELECT pg_catalog.pg_advisory_xact_lock(2147483647, 2147483647)").await.expect("unrelated transaction retains its own lock");
                    let unrelated_pid: i32 = unrelated.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("unrelated backend identity").get(0);
                    let operation_id = operation();
                    let id = grant.id();
                    proxy.arm(Fault::DelayedCommit);
                    let service = services.grant().clone();
                    let actor = holder.clone();
                    let exit = tokio::spawn(async move { service.terminate(actor, operation_id, id).await });
                    let pid = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("lost response before original commit");
                    assert_ne!(pid, unrelated_pid);
                    let suspension = services.suspension().clone();
                    let cancellation = services.cancellation().clone();
                    let account_id = holder.account_id();
                    let confirmation = json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000"});
                    let state = tokio::spawn(async move {
                        if cancelled {
                            let fact=cancellation.cancel_account(manager, operation(), account_id, serde_json::from_value(confirmation).expect("cancellation confirmation")).await.expect("later cancellation");
                            (Some(fact.id()),None)
                        } else {
                            let fact=suspension.suspend(manager, operation(), account_id, serde_json::from_value(confirmation).expect("suspension confirmation")).await.expect("later suspension");
                            (None,Some(fact.id()))
                        }
                    });
                    wait_for_lock(observer.client(), pid, "SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE").await;
                    proxy.release();
                    let (cancellation_id,suspension_id)=timeout(Duration::from_secs(5), state).await.expect("later state deadline").expect("state task");
                    let result = timeout(Duration::from_secs(5), exit).await.expect("recovery deadline").expect("exit task").expect("committed result survives later loss of access");
                    assert_eq!(result.id(), id);
                    assert_eq!(result.operation_id(), operation_id);
                    assert_eq!(result.subject().person_id(), candidate.identity().person_id());
                    assert_eq!(result.sequence(), 2);
                    assert!(matches!(services.current().read(&holder).await, Err(current::Error::Invalid)));
                    assert!(matches!(Box::pin(services.grant().terminate(holder, operation_id, id)).await, Err(Error::Current(current::Error::Invalid))), "a new request still requires a valid current session");
                    let facts=observer.client().query_one(
                        "SELECT (SELECT count(*) FROM access.terminations)=1
                                AND EXISTS(SELECT 1 FROM access.terminations WHERE id=$1 AND person_id=$3 AND operation_id=$2 AND sequence=2)
                                AND NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL)
                                AND (SELECT count(*) FROM access.grants)=1 AND (SELECT count(*) FROM access.assignments)=1 AS participation,
                            EXISTS(SELECT 1 FROM access.grants WHERE id=$1 AND person_id=$3 AND sequence=2 AND termination='self')
                                AND EXISTS(SELECT 1 FROM access.assignments WHERE grant_id=$1 AND person_id=$3 AND sequence=1)
                                AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$3 AND sequence=CASE WHEN $4 THEN 2 ELSE 1 END) AS personal,
                            EXISTS(SELECT 1 FROM identity.accounts account
                                JOIN identity.creations creation ON creation.id=account.id AND creation.person_id=account.person_id
                                WHERE account.id=$5 AND account.person_id=$3 AND creation.sequence=1 AND account.created_at=creation.created_at
                                    AND account.sequence=2 AND account.version=2
                                    AND account.cancellation_id IS NOT DISTINCT FROM $6 AND account.suspension_id IS NOT DISTINCT FROM $7
                                    AND CASE WHEN $4 THEN account.mobile IS NULL AND account.id_number IS NULL
                                        ELSE (account.mobile).value IS NOT NULL AND account.id_number IS NOT NULL END) AS account,
                            ((SELECT count(*) FROM identity.cancellations)=1)=$4
                                AND ((SELECT count(*) FROM identity.suspensions)=1)<>$4
                                AND NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE account_id=$5)
                                AND CASE WHEN $4 THEN EXISTS(SELECT 1 FROM identity.cancellations cancellation
                                    JOIN identity.projections projection ON projection.id=cancellation.id AND projection.cause='cancellation'
                                        AND projection.account_id=cancellation.account_id AND projection.person_id=cancellation.person_id
                                    JOIN identity.accounts account ON account.id=cancellation.account_id
                                    WHERE cancellation.id=$6 AND cancellation.account_id=$5 AND cancellation.person_id=$3 AND cancellation.sequence=2
                                        AND projection.sequence=2 AND projection.operation_id=cancellation.operation_id
                                        AND projection.projected_at=cancellation.cancelled_at AND account.changed_at=cancellation.cancelled_at)
                                ELSE EXISTS(SELECT 1 FROM identity.suspensions suspension JOIN identity.accounts account ON account.id=suspension.account_id
                                    WHERE suspension.id=$7 AND suspension.account_id=$5 AND suspension.person_id=$3 AND suspension.sequence=2
                                        AND account.changed_at=suspension.suspended_at) END AS cause",
                        &[&id,&operation_id,&candidate.identity().person_id(),&cancelled,&account_id,&cancellation_id,&suspension_id],
                    ).await.expect("independent original Person facts and Account-owned terminal transition");
                    for column in ["participation","personal","account","cause"] {assert!(facts.get::<_,bool>(column),"cancelled={cancelled}: {column}");}
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    services.ready().await.expect("both permanent facts survive restart");
                    services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped");
                    assert_eq!(observer.client().query_one("SELECT state FROM pg_stat_activity WHERE pid=$1", &[&unrelated_pid]).await.expect("unrelated transaction survives original recovery").get::<_, String>(0), "idle in transaction");
                    unrelated.client().batch_execute("ROLLBACK").await.expect("unrelated transaction ended");
                    unrelated.terminate().await.expect("unrelated connection ended");
                    observer.terminate().await.expect("observer ended");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("loss of access during recovery deadline");
            }
        }));
    }

    #[test]
    fn self_termination_resolves_commit_faults_and_retains_accepted_work() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::LostCommit, Fault::RolledBack, Fault::UnknownCommit, Fault::ReadCommitTermination, Fault::DelayedCommit] {
                timeout(Duration::from_secs(40), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let holder = register(&database, &services, &mut supplier).await;
                    let candidate = services.grant().candidate(&manager, Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 20).expect("pagination")).await.expect("candidate");
                    let (grant, _) = Box::pin(services.grant().create(manager, operation(), input(&candidate, 0))).await.expect("member admitted");
                    let operation_id = operation();
                    let id = grant.id();
                    let cancelled = matches!(fault, Fault::DelayedCommit);
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    let unknown = matches!(fault, Fault::UnknownCommit);
                    let cleanup = matches!(fault, Fault::ReadCommitTermination);
                    proxy.arm(if cleanup { Fault::DelayedCommit } else { fault });
                    let service = services.grant().clone();
                    let actor = holder.clone();
                    let waiter = tokio::spawn(async move { service.terminate(actor, operation_id, id).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("business commit intercepted");
                    if cleanup { proxy.arm(Fault::ReadCommitTermination); }
                    if cancelled {
                        waiter.abort(); let _ = waiter.await;
                        services.close();
                        assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "cancelled waiter does not release accepted termination work");
                        proxy.release();
                        services.drain().await.expect("accepted work drained");
                    } else {
                        proxy.release();
                        let result = timeout(Duration::from_secs(15), waiter).await.expect("bounded outcome").expect("waiter");
                        if rolled_back { assert!(matches!(result, Err(Error::Command(crate::kernel::Error::Unavailable(ref fault))) if fault.stage()==crate::kernel::Stage::Commit)); }
                        else if unknown { assert!(matches!(result, Err(Error::Command(crate::kernel::Error::Unknown(_))))); }
                        else { assert_eq!(result.expect("confirmed permanent termination").id(), id); }
                        services.close(); services.drain().await.expect("drained");
                    }
                    context.stop().await.expect("stopped");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT id FROM access.grants WHERE id=$1 FOR UPDATE", &[&id]).await.expect("original backend released its Grant lock");
                    let expected = if rolled_back { 0_i64 } else { 1_i64 };
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.grants),(SELECT count(*) FROM access.assignments),(SELECT count(*) FROM access.terminations),(SELECT count(*) FROM access.grants WHERE termination IS NULL),sequence FROM access.grants WHERE id=$1", &[&id]).await.expect("durable paired result after original backend ended");
                    assert_eq!(row.get::<_,i64>(0), 1);
                    assert_eq!(row.get::<_,i64>(1), 1);
                    assert_eq!(row.get::<_,i64>(2), expected);
                    assert_eq!(row.get::<_,i64>(3), 1 - expected);
                    assert_eq!(row.get::<_,i64>(4), 1 + expected);
                    observer.client().batch_execute("COMMIT").await.expect("observation ended");
                    observer.terminate().await.expect("observer ended");
                    proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let termination = Box::pin(services.grant().terminate(holder, operation_id, id)).await.expect("original result or explicit retry after proven rollback");
                    assert_eq!(termination.id(), id);
                    assert_eq!(termination.sequence(), 2);
                    let observer = database.connections(&database.user).recover().await.expect("replay observer");
                    assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.terminations)=1 AND NOT EXISTS(SELECT 1 FROM access.grants WHERE termination IS NULL) AND EXISTS(SELECT 1 FROM access.grants WHERE person_id=$1 AND sequence=2 AND termination='self') AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$1 AND sequence=1)", &[&candidate.identity().person_id()]).await.expect("exactly one termination after replay").get::<_,bool>(0));
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("termination fault scenario deadline");
            }
        }));
    }
}
