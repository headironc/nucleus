//! Candidate disclosure and qualified management projections.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};
use tokio_postgres::Client;
use utoipa::ToSchema;

use crate::{
    access::{appointment, role},
    center::affiliation::{self, Code, Error, Rejection, Request, Service, query},
    history::View,
    identity::{account, current::Actor, mobile::Mobile, operation, person, person::revision},
    kernel::Query,
    pagination::Pagination,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Account(account::Id),
    Person,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = affiliation::management::Input)]
pub struct Input {
    pub(super) target: Confirmation,
    pub(super) affiliation: Code,
}

#[derive(Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[schema(as = affiliation::management::Confirmation)]
pub(super) enum Confirmation {
    #[serde(rename_all = "camelCase")]
    Account {
        account_id: account::Id,
        person_id: person::Id,
        revision_id: revision::Id,
        // A person-ID candidate has no locating mobile; a mobile candidate retains it.
        #[serde(default, deserialize_with = "mobile")]
        #[schema(value_type = String, required = false, nullable = false, pattern = "^1[0-9]{10}$")]
        mobile: Option<Mobile>,
    },
    #[serde(rename_all = "camelCase")]
    Person {
        person_id: person::Id,
        revision_id: revision::Id,
    },
}

fn mobile<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Mobile>, D::Error> {
    Mobile::deserialize(deserializer).map(Some)
}

impl Confirmation {
    pub(super) fn person_id(&self) -> person::Id {
        match self {
            Self::Account { person_id, .. } | Self::Person { person_id, .. } => *person_id,
        }
    }
    pub(super) fn revision_id(&self) -> revision::Id {
        match self {
            Self::Account { revision_id, .. } | Self::Person { revision_id, .. } => *revision_id,
        }
    }
}

pub struct Candidate {
    target: Target,
    identity: revision::Identity,
}

impl Candidate {
    pub fn target(&self) -> Target {
        self.target
    }
    pub fn identity(&self) -> &revision::Identity {
        &self.identity
    }
}

pub struct List {
    pub(super) count: i64,
    pub(super) affiliations: Vec<Affiliation>,
}

impl List {
    pub fn count(&self) -> i64 {
        self.count
    }
    pub fn affiliations(&self) -> &[Affiliation] {
        &self.affiliations
    }
}

pub struct Affiliation {
    pub(super) value: Code,
    pub(super) created_at: DateTime<Utc>,
    pub(super) operator: revision::Identity,
}

impl Affiliation {
    pub fn value(&self) -> Code {
        self.value
    }
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub fn operator(&self) -> &revision::Identity {
        &self.operator
    }
}

impl Service {
    pub async fn correct(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        person_id: person::Id,
        input: Input,
    ) -> Result<(), Error> {
        if input.target.person_id() != person_id {
            return Err(Rejection::TargetChanged.into());
        }
        self.submit(actor, operation_id, Request::Correct { input })
            .await
    }

    async fn authorize_candidate(
        &self,
        client: &Client,
        actor: &Actor,
        view: View,
    ) -> Result<(role::Role, person::Id, DateTime<Utc>), Error> {
        let (person, _, now) = self.current.at(view).fetch_person(client, actor).await?;
        let current =
            appointment::find_by_account(client, &self.roles.at(view), actor.account_id(), now)
                .await?
                .ok_or(Error::corrupt(
                    "the actor retains its institutional assignment",
                ))?;
        if !current
            .role()
            .permissions()
            .contains(&role::Code::InstitutionAffiliationsRead)
            && !current
                .role()
                .permissions()
                .contains(&role::Code::InstitutionAffiliationsCorrect)
        {
            return Err(Error::Denied);
        }
        Ok((current.into_role(), person, now))
    }

    async fn authorize_account(
        client: &Client,
        roles: &role::Reader,
        role: &role::Role,
        account: account::Id,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        if !role
            .permissions()
            .contains(&role::Code::InstitutionAffiliationsRead)
        {
            let target = appointment::find_by_account(client, roles, account, now)
                .await?
                .ok_or(Error::corrupt(
                    "the located account retains its institutional assignment",
                ))?;
            if target.role().level() >= role.level() {
                return Err(Rejection::Absent.into());
            }
        }
        Ok(())
    }

    pub async fn candidate_by_mobile(
        &self,
        actor: &Actor,
        mobile: Mobile,
    ) -> Result<Candidate, Error> {
        self.app
            .query(&CandidateQuery {
                service: self,
                actor,
                target: Locator::Mobile(&mobile),
            })
            .await
    }

    pub async fn candidate_by_person(
        &self,
        actor: &Actor,
        person_id: person::Id,
    ) -> Result<Candidate, Error> {
        self.app
            .query(&CandidateQuery {
                service: self,
                actor,
                target: Locator::Person(person_id),
            })
            .await
    }

    pub async fn read_person(
        &self,
        actor: &Actor,
        person_id: person::Id,
    ) -> Result<affiliation::Affiliation, Error> {
        self.app
            .query(&query::Current {
                service: self,
                actor,
                target: Some(person_id),
            })
            .await
    }

    pub async fn history_by_person(
        &self,
        actor: &Actor,
        person_id: person::Id,
        pagination: Pagination,
    ) -> Result<List, Error> {
        self.app
            .query(&query::ManagementHistory {
                service: self,
                actor,
                person: person_id,
                pagination,
            })
            .await
    }
}

enum Locator<'a> {
    Mobile(&'a Mobile),
    Person(person::Id),
}

struct CandidateQuery<'a> {
    service: &'a Service,
    actor: &'a Actor,
    target: Locator<'a>,
}

impl Query for CandidateQuery<'_> {
    type Authorized = (role::Role, person::Id, DateTime<Utc>);
    type Output = Candidate;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<Self::Authorized, Error> {
        self.service
            .authorize_candidate(client, self.actor, view)
            .await
    }

    async fn read(
        &self,
        client: &Client,
        (role, operator, now): Self::Authorized,
        view: View,
    ) -> Result<Candidate, Error> {
        match self.target {
            Locator::Mobile(mobile) => {
                let account = self
                    .service
                    .accounts
                    .at(view)
                    .find_by_mobile(client, mobile, now)
                    .await?
                    .ok_or(Rejection::Absent)?;
                if account == self.actor.account_id() {
                    return Err(Rejection::Absent.into());
                }
                Service::authorize_account(
                    client,
                    &self.service.roles.at(view),
                    &role,
                    account,
                    now,
                )
                .await?;
                let subject = self
                    .service
                    .accounts
                    .at(view)
                    .find_by_id(client, account, mobile, now)
                    .await?
                    .ok_or(Rejection::Absent)?;
                Ok(Candidate {
                    target: Target::Account(account),
                    identity: subject.into(),
                })
            }
            Locator::Person(person) => {
                if person == operator {
                    return Err(Rejection::Absent.into());
                }
                let target = match self
                    .service
                    .accounts
                    .at(view)
                    .find_by_person(client, person, now)
                    .await?
                {
                    Some(account) => {
                        Service::authorize_account(
                            client,
                            &self.service.roles.at(view),
                            &role,
                            account,
                            now,
                        )
                        .await?;
                        Target::Account(account)
                    }
                    None => Target::Person,
                };
                let identity = self
                    .service
                    .persons
                    .with_read(self.service.roles.at(view).read()?)
                    .find_identity(client, person, now)
                    .await?
                    .ok_or(Rejection::Absent)?;
                Ok(Candidate { target, identity })
            }
        }
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
            role,
            role::tests::{actor, start},
        },
        account::tests::{assign, register_manager},
        center::affiliation::{self, Error, Rejection, Target},
        history,
        identity::{
            authentication::{Database, Supplier, server},
            current::{self, Actor},
            operation, person,
        },
        kernel::{FaultKind, Stage},
        pagination::Pagination,
        startup::Services,
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    async fn create_role(
        services: &Services,
        actor: &Actor,
        name: &str,
        level: i16,
        permissions: &[&str],
    ) -> role::Role {
        Box::pin(
            services.role().create(
                actor.clone(),
                operation(),
                serde_json::from_value(
                    json!({"name":name,"level":level,"permissions":permissions}),
                )
                .expect("role input"),
            ),
        )
        .await
        .expect("actual role creation")
    }

    fn correction(account: &current::Account, code: &str) -> affiliation::management::Input {
        serde_json::from_value(json!({
            "target": {"type":"account", "accountId":account.id(), "personId":account.person_id(), "revisionId":account.revision_id()},
            "affiliation": code,
        })).expect("confirmed correction input")
    }

    #[test]
    fn correction_input_retains_typed_confirmations_and_rejects_invalid_supplied_values() {
        let id = Uuid::new_v4();
        for target in [
            json!({"type":"account","accountId":id,"personId":id,"revisionId":id}),
            json!({"type":"account","accountId":id,"personId":id,"revisionId":id,"mobile":"13800000000"}),
            json!({"type":"person","personId":id,"revisionId":id}),
        ] {
            assert!(
                serde_json::from_value::<affiliation::management::Input>(
                    json!({"target":target,"affiliation":"13"})
                )
                .is_ok()
            );
        }
        for target in [
            json!({"type":"account","accountId":id,"personId":id,"revisionId":id,"mobile":null}),
            json!({"type":"account","accountId":id,"personId":id,"revisionId":id,"mobile":"invalid-private-mobile"}),
            json!({"type":"account","accountId":"invalid","personId":id,"revisionId":id}),
            json!({"type":"account","accountId":id,"personId":id}),
            json!({"type":"person","personId":id,"revisionId":id,"mobile":"13800000000"}),
            json!({"type":"person","personId":id,"revisionId":null}),
            json!({"type":"unknown","personId":id,"revisionId":id}),
        ] {
            let error = serde_json::from_value::<affiliation::management::Input>(
                json!({"target":target,"affiliation":"13"}),
            )
            .err()
            .expect("invalid confirmation");
            assert!(!error.to_string().contains("invalid-private-mobile"));
        }
    }

    #[test]
    fn correction_appends_same_values_and_keeps_original_people_after_cancellation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context,services) = start(database.connections(&database.user),&supplier).await;
                let administrator = actor(&database,&services,&mut supplier).await;
                let operator = services.current().read(&administrator).await.expect("operator");
                let member = register(&database,&services,&mut supplier).await;
                let target = services.current().read(&member).await.expect("target");
                let original = operation();
                services.affiliation().correct(administrator.clone(),original,target.person_id(),correction(&target,"01")).await.expect("management can fill the first value");
                services.affiliation().correct(administrator.clone(),operation(),target.person_id(),correction(&target,"01")).await.expect("same-value correction appends");
                services.affiliation().correct(administrator.clone(),original,target.person_id(),correction(&target,"01")).await.expect("original completion is not a third append");
                assert!(matches!(services.affiliation().create(member.clone(),operation(),serde_json::from_value(json!({"affiliation":"13"})).expect("self fill")).await,Err(Error::Rejected(Rejection::Conflict))));
                assert!(matches!(services.affiliation().correct(administrator.clone(),original,target.person_id(),correction(&target,"13")).await,Err(Error::Mismatch)));
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let rows = observer.client().query("SELECT person_id,operator_id,created_at,operation_id,octet_length(value) FROM center.affiliations ORDER BY created_at",&[]).await.expect("permanent facts");
                assert_eq!(rows.len(),2);
                for row in &rows {
                    assert_eq!(row.get::<_,person::Id>(0),target.person_id());
                    assert_eq!(row.get::<_,person::revision::Id>(1),operator.revision_id());
                    assert_eq!(row.get::<_,i32>(4),39);
                }
                assert!(rows[0].get::<_,chrono::DateTime<chrono::Utc>>(2)<rows[1].get::<_,chrono::DateTime<chrono::Utc>>(2));
                assert_eq!(rows[0].get::<_,operation::Id>(3),original);
                let own = services.affiliation().history(&member,Pagination::new(1,20).expect("page")).await.expect("self reads managed facts");
                assert_eq!(own.count(),2);
                let history = services.affiliation().history_by_person(&administrator,target.person_id(),Pagination::new(1,20).expect("page")).await.expect("fixed management operator");
                assert!(history.affiliations().iter().all(|item|item.operator().person_id()==operator.person_id() && item.operator().revision_id()==operator.revision_id()));
                Box::pin(services.cancellation().cancel(member.clone(),operation(),serde_json::from_value(json!({"accountId":target.id(),"personId":target.person_id(),"revisionId":target.revision_id()})).expect("cancellation"))).await.expect("actual target cancellation");
                let candidate = services.affiliation().candidate_by_person(&administrator,target.person_id()).await.expect("permanent person candidate");
                assert_eq!(candidate.target(),Target::Person);
                assert!(matches!(services.affiliation().correct(administrator.clone(),operation(),target.person_id(),correction(&target,"13")).await,Err(Error::Rejected(Rejection::TargetChanged))),"a new operation cannot reuse the cancelled account confirmation");
                services.affiliation().correct(administrator.clone(),operation(),target.person_id(),serde_json::from_value(json!({"target":{"type":"person","personId":target.person_id(),"revisionId":target.revision_id()},"affiliation":"13"})).expect("cancelled target")).await.expect("management corrects a person without an account");
                services.affiliation().correct(administrator.clone(),original,target.person_id(),correction(&target,"01")).await.expect("replay retains the original target across cancellation");
                let replacement = register(&database,&services,&mut supplier).await;
                let replacement = services.current().read(&replacement).await.expect("new same-mobile person");
                assert_ne!(replacement.person_id(),target.person_id());
                assert!(matches!(services.affiliation().read_person(&administrator,replacement.person_id()).await,Err(Error::Rejected(Rejection::Unset))));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM center.affiliations WHERE person_id=$1",&[&target.person_id()]).await.expect("three permanent rows").get::<_,i64>(0),3);
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context,services) = start(database.connections(&database.user),&supplier).await;
                services.ready().await.expect("startup accepts legitimate management operators");
                services.close(); services.drain().await.expect("restarted services drained"); context.stop().await.expect("restarted context stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded correction lifecycle");
        }));
    }

    #[test]
    fn correction_rechecks_confirmation_but_replays_the_original_person_and_value() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35),Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context,services) = start(database.connections(&database.user),&supplier).await;
                let administrator = actor(&database,&services,&mut supplier).await;
                let member = register(&database,&services,&mut supplier).await;
                let target = services.current().read(&member).await.expect("confirmed target");
                let confirmed = json!({"type":"account","accountId":target.id(),"personId":target.person_id(),"revisionId":target.revision_id(),"mobile":"13800000000"});
                let original = operation();
                services.affiliation().correct(administrator.clone(),original,target.person_id(),serde_json::from_value(json!({"target":confirmed,"affiliation":"01"})).expect("mobile confirmation")).await.expect("complete target confirmation");
                for (field,value) in [("accountId",json!(Uuid::new_v4())),("revisionId",json!(Uuid::new_v4())),("mobile",json!("13800000001"))] {
                    let mut changed = confirmed.clone(); changed[field]=value;
                    assert!(matches!(services.affiliation().correct(administrator.clone(),operation(),target.person_id(),serde_json::from_value(json!({"target":changed,"affiliation":"13"})).expect("well-shaped stale confirmation")).await,Err(Error::Rejected(Rejection::TargetChanged))),"{field}");
                }
                assert!(matches!(services.affiliation().correct(administrator.clone(),operation(),person::Id::allocate(),correction(&target,"13")).await,Err(Error::Rejected(Rejection::TargetChanged))),"path and confirmation must identify the same person");
                assert!(matches!(services.affiliation().correct(administrator.clone(),operation(),target.person_id(),serde_json::from_value(json!({"target":{"type":"person","personId":target.person_id(),"revisionId":target.revision_id()},"affiliation":"13"})).expect("wrong target form")).await,Err(Error::Rejected(Rejection::TargetChanged))));
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let snapshot = "SELECT jsonb_agg(to_jsonb(affiliation) ORDER BY created_at)::text FROM center.affiliations affiliation";
                let before: String = observer.client().query_one(snapshot,&[]).await.expect("only the original value persists").get(0);
                assert_eq!(observer.client().query_one("SELECT count(*) FROM center.affiliations",&[]).await.expect("rejected confirmations add nothing").get::<_,i64>(0),1);
                Box::pin(services.correction().correct(administrator.clone(),operation(),serde_json::from_value(json!({"target":{"personId":target.person_id(),"revisionId":target.revision_id()},"name":"Changed Person","idNumber":"00000020000229007X"})).expect("identity correction"))).await.expect("actual target revision change");
                assert!(matches!(services.affiliation().correct(administrator.clone(),operation(),target.person_id(),correction(&target,"13")).await,Err(Error::Rejected(Rejection::TargetChanged))));
                services.affiliation().correct(administrator.clone(),original,target.person_id(),correction(&target,"01")).await.expect("original people and code survive a later revision");
                assert!(matches!(services.affiliation().correct(administrator.clone(),original,target.person_id(),correction(&target,"13")).await,Err(Error::Mismatch)));
                assert_eq!(observer.client().query_one(snapshot,&[]).await.expect("replay and rejections preserve original bytes").get::<_,String>(0),before);
                let revised = services.current().read(&member).await.expect("new target revision");
                services.affiliation().correct(administrator.clone(),operation(),revised.person_id(),correction(&revised,"13")).await.expect("newly confirmed target");
                assert_eq!(serde_json::to_value(services.affiliation().read_person(&administrator,target.person_id()).await.expect("latest value")).expect("projection")["value"],"13");
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded correction confirmations");
        }));
    }

    #[test]
    fn concurrent_corrections_serialize_with_self_fill_and_preserve_every_managed_value() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30),Box::pin(async {
                let database=Database::initialized(Arc::clone(&server)).await; let mut supplier=Supplier::new().await;
                let proxy=Proxy::start(&server).await;
                let (context,services)=start(crate::identity::authentication::proxied(&database,&proxy),&supplier).await;
                let administrator=actor(&database,&services,&mut supplier).await;
                let member=register(&database,&services,&mut supplier).await;
                let target=services.current().read(&member).await.expect("target");
                let first_id=operation(); let second_id=operation();
                let service=services.affiliation().clone(); let operator=administrator.clone(); let input=correction(&target,"01"); let person_id=target.person_id();
                proxy.arm(Fault::DelayedCommit);
                let first=tokio::spawn(async move { service.correct(operator,first_id,person_id,input).await });
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("first managed write awaits commit");
                let observer=database.connections(&database.user).recover().await.expect("independent observer");
                let pid = proxy.intercepted().await;
                let service=services.affiliation().clone(); let own=member.clone();
                let first_fill=tokio::spawn(async move { service.create(own,operation(),serde_json::from_value(json!({"affiliation":"13"})).expect("self fill")).await });
                let service=services.affiliation().clone(); let input=correction(&target,"13"); let operator=administrator.clone();
                let second=tokio::spawn(async move { service.correct(operator,second_id,person_id,input).await });
                timeout(Duration::from_secs(3),async {
                    loop {
                        if observer.client().query_one("WITH RECURSIVE waiting AS (SELECT pid,pg_blocking_pids(pid) AS blockers FROM pg_stat_activity WHERE datname=current_database()), blocked AS (SELECT pid FROM waiting WHERE $1=ANY(blockers) UNION SELECT waiting.pid FROM waiting JOIN blocked ON blocked.pid=ANY(waiting.blockers)) SELECT count(*) FROM blocked JOIN pg_stat_activity activity USING (pid) WHERE wait_event_type='Lock' AND (query LIKE 'SELECT person.id FROM identity.persons person%' OR query LIKE 'SELECT id FROM identity.persons WHERE id%')",&[&pid]).await.expect("both consumers wait behind the original person lock, including queued tuple locks").get::<_,i64>(0)==2 { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("observed competing person locks");
                proxy.release(); first.await.expect("first waiter").expect("first managed value");
                assert!(matches!(first_fill.await.expect("self waiter"),Err(Error::Rejected(Rejection::Conflict))));
                second.await.expect("second waiter").expect("second managed value");
                let rows=observer.client().query("SELECT operation_id,created_at FROM center.affiliations WHERE person_id=$1 ORDER BY created_at",&[&person_id]).await.expect("independent permanent order");
                assert_eq!(rows.len(),2); assert_eq!(rows[0].get::<_,operation::Id>(0),first_id); assert_eq!(rows[1].get::<_,operation::Id>(0),second_id);
                assert!(rows[0].get::<_,chrono::DateTime<chrono::Utc>>(1)<rows[1].get::<_,chrono::DateTime<chrono::Utc>>(1));
                assert_eq!(serde_json::to_value(services.affiliation().read(&member).await.expect("latest committed value")).expect("projection")["value"],"13");
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded concurrent correction");
        }));
    }

    #[test]
    fn correction_recovery_keeps_both_accepted_people_after_waiter_or_permission_changes() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["revoked","permission","cancelled","operator","target"] {
                timeout(Duration::from_secs(30),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await; let mut supplier=Supplier::new().await;
                    let proxy=Proxy::start(&server).await;
                    let (context,services)=start(crate::identity::authentication::proxied(&database,&proxy),&supplier).await;
                    let administrator=actor(&database,&services,&mut supplier).await; let system=services.current().read(&administrator).await.expect("system operator");
                    let manager=Box::pin(register_manager(&database,&services,&mut supplier)).await; let operator=services.current().read(&manager).await.expect("accepted operator");
                    let selected=create_role(&services,&administrator,"Corrector",20,&["institution.affiliations.correct"]).await;
                    Box::pin(assign(&services,&administrator,"13700000000",&selected)).await;
                    let member=register(&database,&services,&mut supplier).await; let target=services.current().read(&member).await.expect("accepted subject");
                    let original=operation(); let service=services.affiliation().clone(); let accepted=manager.clone(); let input=correction(&target,"01"); let person_id=target.person_id();
                    proxy.arm(Fault::DelayedCommit);
                    let mut waiter=tokio::spawn(async move { service.correct(accepted,original,person_id,input).await });
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("original correction awaits commit confirmation");
                    if case=="cancelled" { waiter.abort(); assert!((&mut waiter).await.expect_err("caller ended its wait").is_cancelled()); }
                    proxy.arm(Fault::StalledAffiliation); proxy.release();
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("independent original-result recovery is waiting");
                    let owner=database.connections("postgres").recover().await.expect("isolated observer and fault owner");
                    let row=owner.client().query_one("SELECT person_id,operator_id FROM center.affiliations WHERE operation_id=$1",&[&original]).await.expect("committed fact before recovery or replay");
                    assert_eq!(row.get::<_,person::Id>(0),target.person_id()); assert_eq!(row.get::<_,person::revision::Id>(1),operator.revision_id());
                    match case {
                        "revoked" | "cancelled" => {
                            services.revocation().revoke(manager.clone(),operation(),crate::identity::revocation::Scope::Current).await.expect("actual revocation before recovery reads");
                            assert!(owner.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1)",&[&manager.session_id()]).await.expect("independent revocation fact").get::<_,bool>(0));
                        }
                        "permission" => {
                            Box::pin(services.role().update(administrator.clone(),operation(),selected.id(),serde_json::from_value(json!({"changeId":selected.change_id(),"name":"Corrector","level":20,"permissions":[]})).expect("permission removal"))).await.expect("actual loss of correction authority");
                        }
                        "operator" | "target" => {
                            owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated accepted-binding damage");
                            if case=="operator" {
                                owner.client().execute("UPDATE center.affiliations SET operator_id=$2 WHERE operation_id=$1",&[&original,&system.revision_id()]).await.expect("different existing operator");
                            } else {
                                owner.client().execute("UPDATE center.affiliations SET person_id=$2 WHERE operation_id=$1",&[&original,&operator.person_id()]).await.expect("different existing subject");
                            }
                            owner.client().batch_execute("COMMIT").await.expect("damage visible before recovery");
                        }
                        _ => unreachable!("fixed fault cases"),
                    }
                    if case=="cancelled" {
                        services.close(); assert!(timeout(Duration::from_millis(50),services.drain()).await.is_err(),"cancelled caller does not release the unfinished recovery");
                    }
                    proxy.release();
                    if case!="cancelled" {
                        let result=waiter.await.expect("original waiter");
                        if matches!(case,"operator" | "target") {
                            assert!(matches!(result,Err(Error::Unknown(_)))); assert!(services.ready().await.is_err(),"accepted binding damage closes admission");
                        } else {
                            result.expect("recovery uses accepted authority without reexecution");
                            assert_eq!(services.affiliation().correct(manager,original,target.person_id(),correction(&target,"01")).await.expect_err("a new request requires current authority").code(),if case=="permission" {Some("permission_denied")} else {Some("authentication_invalid")});
                        }
                    }
                    services.close(); services.drain().await.expect("original work and recovery drained");
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM center.affiliations",&[]).await.expect("no repeated correction").get::<_,i64>(0),1);
                    owner.terminate().await.expect("owner ended"); context.stop().await.expect("stopped"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded managed recovery case");
            }
        }));
    }

    #[test]
    fn correction_requires_its_exact_permission_and_higher_level_without_an_original_value_oracle()
    {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context,services) = start(database.connections(&database.user),&supplier).await;
                let administrator = actor(&database,&services,&mut supplier).await;
                let manager = Box::pin(register_manager(&database,&services,&mut supplier)).await;
                let member = register(&database,&services,&mut supplier).await;
                let target = services.current().read(&member).await.expect("target");
                let own = services.current().read(&manager).await.expect("manager");
                let system = services.current().read(&administrator).await.expect("system");
                let original = operation();
                services.affiliation().correct(administrator.clone(),original,target.person_id(),correction(&target,"01")).await.expect("another operator's original value");
                let mut selected = create_role(&services,&administrator,"Corrector",20,&[]).await;
                Box::pin(assign(&services,&administrator,"13700000000",&selected)).await;
                let lower = create_role(&services,&administrator,"Lower",10,&[]).await;
                let equal = create_role(&services,&administrator,"Equal",20,&[]).await;
                let higher = create_role(&services,&administrator,"Higher",30,&[]).await;
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let mut count = 1_i64;
                for permissions in [vec!["institution.accounts"],vec![],vec!["institution.affiliations.read"],vec!["institution.affiliations.correct","institution.affiliations.read"],vec!["institution.affiliations.correct"]] {
                    selected = Box::pin(services.role().update(administrator.clone(),operation(),selected.id(),serde_json::from_value(json!({"changeId":selected.change_id(),"name":"Corrector","level":20,"permissions":permissions})).expect("permissions"))).await.expect("actual permission change");
                    let allowed = permissions.contains(&"institution.affiliations.correct");
                    for target_role in [&lower,&equal,&higher] {
                        Box::pin(assign(&services,&administrator,"13800000000",target_role)).await;
                        let result = services.affiliation().correct(manager.clone(),operation(),target.person_id(),correction(&target,"01")).await;
                        if allowed && target_role.level()<selected.level() {
                            result.expect("strictly lower target"); count+=1;
                        } else {
                            assert_eq!(result.expect_err("no inferred write authority").code(),if allowed { Some("target_unavailable") } else { Some("permission_denied") });
                        }
                        assert_eq!(observer.client().query_one("SELECT count(*) FROM center.affiliations",&[]).await.expect("successful appends only").get::<_,i64>(0),count);
                    }
                }
                for unavailable in [&own,&system] {
                    assert!(matches!(services.affiliation().correct(manager.clone(),operation(),unavailable.person_id(),correction(unavailable,"13")).await,Err(Error::Rejected(Rejection::Absent))));
                }
                Box::pin(assign(&services,&administrator,"13800000000",&lower)).await;
                let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("controlled original ciphertext damage");
                owner.client().execute("UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38)#1) WHERE operation_id=$1",&[&original]).await.expect("bad original tag");
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                for value in 1..=13 {
                    assert!(matches!(services.affiliation().correct(manager.clone(),original,target.person_id(),correction(&target,&format!("{value:02}"))).await,Err(Error::Mismatch)),"another operator is rejected before opening any code");
                }
                services.ready().await.expect("unauthorized originals never decode the damaged value");
                assert_eq!(services.affiliation().read_person(&manager,target.person_id()).await.err().expect("correction alone cannot disclose values").code(),Some("permission_denied"));
                Box::pin(services.suspension().suspend(administrator.clone(),operation(),target.id(),serde_json::from_value(json!({"personId":target.person_id(),"revisionId":target.revision_id(),"mobile":"13800000000"})).expect("target suspension"))).await.expect("actual target suspension");
                services.affiliation().correct(manager.clone(),operation(),target.person_id(),correction(&target,"13")).await.expect("paused targets remain manageable");
                count+=1;
                Box::pin(services.suspension().suspend(administrator,operation(),own.id(),serde_json::from_value(json!({"personId":own.person_id(),"revisionId":own.revision_id(),"mobile":"13700000000"})).expect("operator suspension"))).await.expect("actual operator suspension");
                assert_eq!(services.affiliation().correct(manager,operation(),target.person_id(),correction(&target,"13")).await.expect_err("suspended operator cannot continue").code(),Some("authentication_invalid"));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM center.affiliations",&[]).await.expect("all rejected operations left facts unchanged").get::<_,i64>(0),count);
                owner.terminate().await.expect("owner ended"); observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded correction permission matrix");
        }));
    }

    #[test]
    fn correction_checks_authority_before_disclosing_a_missing_person() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25),Box::pin(async {
                let database=Database::initialized(server).await; let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let administrator=actor(&database,&services,&mut supplier).await;
                let member=register(&database,&services,&mut supplier).await;
                let missing=person::Id::allocate();
                for (actor,expected) in [(member,Some("permission_denied")),(administrator,Some("target_changed"))] {
                    let input=serde_json::from_value(json!({"target":{"type":"person","personId":missing,"revisionId":Uuid::new_v4()},"affiliation":"01"})).expect("unknown target confirmation");
                    assert_eq!(services.affiliation().correct(actor,operation(),missing,input).await.expect_err("missing target").code(),expected,"current correction authority precedes target disclosure");
                }
                let observer=database.connections(&database.user).recover().await.expect("independent observer");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM center.affiliations",&[]).await.expect("no rejected writes").get::<_,i64>(0),0);
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded correction rejection order");
        }));
    }

    #[test]
    fn management_separates_value_disclosure_from_candidate_qualification() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let manager = Box::pin(register_manager(&database, &services, &mut supplier)).await;
                let member = register(&database, &services, &mut supplier).await;
                let target = services.current().read(&member).await.expect("target");
                let own = services.current().read(&manager).await.expect("manager");
                let system = services.current().read(&administrator).await.expect("system administrator");
                services.affiliation().create(member, operation(), serde_json::from_value(json!({"affiliation":"01"})).expect("first fill")).await.expect("actual target value");
                services.affiliation().create(administrator.clone(), operation(), serde_json::from_value(json!({"affiliation":"13"})).expect("first fill")).await.expect("actual system value");
                let mut selected = create_role(&services, &administrator, "Reader", 20, &[]).await;
                Box::pin(assign(&services, &administrator, "13700000000", &selected)).await;
                let lower = create_role(&services, &administrator, "Lower", 10, &[]).await;
                let equal = create_role(&services, &administrator, "Equal", 20, &[]).await;
                let higher = create_role(&services, &administrator, "Higher", 30, &[]).await;
                for permissions in [vec!["institution.accounts"], vec!["institution.affiliations.correct"], vec!["institution.affiliations.read"], vec!["institution.affiliations.correct","institution.affiliations.read"], vec![]] {
                    selected = Box::pin(services.role().update(administrator.clone(), operation(), selected.id(), serde_json::from_value(json!({"changeId":selected.change_id(),"name":"Reader","level":20,"permissions":permissions})).expect("role update"))).await.expect("actual current permission change");
                    let read = permissions.contains(&"institution.affiliations.read");
                    let correct = permissions.contains(&"institution.affiliations.correct");
                    for target_role in [&lower, &equal, &higher] {
                        Box::pin(assign(&services, &administrator, "13800000000", target_role)).await;
                        let visible = read || correct && target_role.level() < selected.level();
                        for result in [services.affiliation().candidate_by_mobile(&manager, "13800000000".parse().expect("mobile")).await, services.affiliation().candidate_by_person(&manager, target.person_id()).await] {
                            if visible {
                                let candidate = result.expect("qualified masked candidate");
                                assert_eq!(candidate.target(), Target::Account(target.id()));
                                assert_eq!(candidate.identity().person_id(), target.person_id());
                                assert_eq!(candidate.identity().revision_id(), target.revision_id());
                                assert_eq!(candidate.identity().name(), "T*");
                            } else {
                                assert_eq!(result.err().expect("hidden target").code(), if correct { Some("target_unavailable") } else { Some("permission_denied") });
                            }
                        }
                        let result = services.affiliation().read_person(&manager, target.person_id()).await;
                        if read { assert_eq!(serde_json::to_value(result.expect("read independent of grade")).expect("projection")["value"], "01"); }
                        else { assert_eq!(result.err().expect("no inferred read right").code(), Some("permission_denied")); }
                        let result = services.affiliation().history_by_person(&manager, target.person_id(), Pagination::new(1,20).expect("page")).await;
                        if read {
                            let list = result.expect("history independent of target grade");
                            assert_eq!(list.count(),1); assert_eq!(list.affiliations().len(),1);
                            assert_eq!(list.affiliations()[0].value(),affiliation::Code::CommunistParty);
                            assert_eq!(list.affiliations()[0].operator().revision_id(),target.revision_id());
                        } else { assert_eq!(result.err().expect("no inferred history right").code(),Some("permission_denied")); }
                    }
                    for (id, mobile) in [(own.person_id(), "13700000000"), (system.person_id(), "13900000000")] {
                        let visible = read && id != own.person_id();
                        for result in [services.affiliation().candidate_by_person(&manager,id).await, services.affiliation().candidate_by_mobile(&manager,mobile.parse().expect("mobile")).await] {
                            if visible { assert_eq!(result.expect("higher system identity").identity().person_id(),id); }
                            else { assert_eq!(result.err().expect("hidden or forbidden").code(), if read || correct { Some("target_unavailable") } else { Some("permission_denied") }); }
                        }
                        let result = services.affiliation().read_person(&manager,id).await;
                        if visible { assert_eq!(serde_json::to_value(result.expect("higher system value")).expect("projection")["value"], "13"); }
                        else { assert_eq!(result.err().expect("self or forbidden").code(), if read { Some("target_unavailable") } else { Some("permission_denied") }); }
                        let result = services.affiliation().history_by_person(&manager,id,Pagination::new(1,20).expect("page")).await;
                        if visible { assert_eq!(result.expect("higher system history").affiliations()[0].value(),affiliation::Code::Unaffiliated); }
                        else { assert_eq!(result.err().expect("self history is not management history").code(),if read { Some("target_unavailable") } else { Some("permission_denied") }); }
                    }
                }
                Box::pin(services.suspension().suspend(administrator, operation(), own.id(), serde_json::from_value(json!({"personId":own.person_id(),"revisionId":own.revision_id(),"mobile":"13700000000"})).expect("suspension"))).await.expect("actual actor suspension");
                assert_eq!(services.affiliation().candidate_by_person(&manager,target.person_id()).await.err().expect("suspended actor").code(), Some("authentication_invalid"));
                assert_eq!(services.affiliation().read_person(&manager,target.person_id()).await.err().expect("suspended actor").code(), Some("authentication_invalid"));
                assert_eq!(services.affiliation().history_by_person(&manager,target.person_id(),Pagination::new(1,20).expect("page")).await.err().expect("suspended history reader").code(),Some("authentication_invalid"));
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded permission matrix");
        }));
    }

    #[test]
    fn management_retains_cancelled_people_without_transferring_their_values() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let original = services.current().read(&member).await.expect("original person");
                assert!(matches!(services.affiliation().read_person(&administrator,original.person_id()).await, Err(Error::Rejected(Rejection::Unset))));
                let list = services.affiliation().history_by_person(&administrator,original.person_id(),Pagination::new(1,20).expect("page")).await.expect("known unfilled person");
                assert_eq!(list.count(),0); assert!(list.affiliations().is_empty());
                let missing = person::Id::allocate();
                assert!(matches!(services.affiliation().read_person(&administrator,missing).await, Err(Error::Rejected(Rejection::Absent))));
                assert!(matches!(services.affiliation().candidate_by_person(&administrator,missing).await, Err(Error::Rejected(Rejection::Absent))));
                assert!(matches!(services.affiliation().history_by_person(&administrator,missing,Pagination::new(1,20).expect("page")).await,Err(Error::Rejected(Rejection::Absent))));
                let operation_id = operation();
                services.affiliation().create(member.clone(),operation_id,serde_json::from_value(json!({"affiliation":"01"})).expect("fill")).await.expect("actual first fill");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let snapshot = "SELECT to_jsonb(affiliation)::text FROM center.affiliations affiliation WHERE operation_id=$1";
                let before: String = observer.client().query_one(snapshot,&[&operation_id]).await.expect("independent original bytes").get(0);
                Box::pin(services.correction().correct(administrator.clone(),operation(),serde_json::from_value(json!({"target":{"personId":original.person_id(),"revisionId":original.revision_id()},"name":"Changed Person","idNumber":"00000020000229007X"})).expect("correction"))).await.expect("actual correction");
                let revised = services.current().read(&member).await.expect("new revision");
                for candidate in [services.affiliation().candidate_by_person(&administrator,original.person_id()).await.expect("person route"),services.affiliation().candidate_by_mobile(&administrator,"13800000000".parse().expect("mobile")).await.expect("mobile route")] {
                    assert_eq!(candidate.identity().name(),"C*"); assert_eq!(candidate.identity().revision_id(),revised.revision_id());
                    assert_eq!(candidate.target(),Target::Account(original.id()));
                }
                Box::pin(services.suspension().suspend(administrator.clone(),operation(),revised.id(),serde_json::from_value(json!({"personId":revised.person_id(),"revisionId":revised.revision_id(),"mobile":"13800000000"})).expect("suspension"))).await.expect("actual target suspension");
                assert!(services.affiliation().candidate_by_person(&administrator,revised.person_id()).await.is_ok());
                assert!(services.affiliation().candidate_by_mobile(&administrator,"13800000000".parse().expect("mobile")).await.is_ok());
                assert_eq!(serde_json::to_value(services.affiliation().read_person(&administrator,revised.person_id()).await.expect("suspended target value")).expect("projection")["value"],"01");
                let list = services.affiliation().history_by_person(&administrator,revised.person_id(),Pagination::new(1,20).expect("page")).await.expect("suspended target history");
                assert_eq!(list.count(),1); assert_eq!(list.affiliations()[0].operator().revision_id(),original.revision_id());
                assert_eq!(list.affiliations()[0].operator().name(),"T*");
                Box::pin(services.cancellation().cancel_account(administrator.clone(),operation(),revised.id(),serde_json::from_value(json!({"personId":revised.person_id(),"revisionId":revised.revision_id(),"mobile":"13800000000"})).expect("cancellation"))).await.expect("actual target cancellation");
                let candidate = services.affiliation().candidate_by_person(&administrator,revised.person_id()).await.expect("permanent person");
                assert_eq!(candidate.target(),Target::Person); assert_eq!(candidate.identity().revision_id(),revised.revision_id());
                assert!(services.affiliation().read_person(&administrator,revised.person_id()).await.is_ok());
                assert!(matches!(services.affiliation().candidate_by_mobile(&administrator,"13800000000".parse().expect("mobile")).await,Err(Error::Rejected(Rejection::Absent))));
                let replacement = register(&database,&services,&mut supplier).await;
                let new_person = services.current().read(&replacement).await.expect("new identity");
                assert_ne!(new_person.person_id(),revised.person_id());
                assert!(matches!(services.affiliation().read_person(&administrator,new_person.person_id()).await,Err(Error::Rejected(Rejection::Unset))));
                assert_eq!(services.affiliation().candidate_by_mobile(&administrator,"13800000000".parse().expect("mobile")).await.expect("new candidate").identity().person_id(),new_person.person_id());
                let list = services.affiliation().history_by_person(&administrator,new_person.person_id(),Pagination::new(1,20).expect("page")).await.expect("new person does not inherit history");
                assert_eq!(list.count(),0); assert!(list.affiliations().is_empty());
                let list = services.affiliation().history_by_person(&administrator,original.person_id(),Pagination::new(1,20).expect("page")).await.expect("cancelled original person's history");
                assert_eq!(list.count(),1); assert_eq!(list.affiliations().len(),1);
                let affiliation = &list.affiliations()[0];
                assert_eq!(affiliation.operator().person_id(),original.person_id());
                assert_eq!(affiliation.operator().revision_id(),original.revision_id());
                assert_eq!(affiliation.operator().name(),"T*");
                let row = observer.client().query_one("SELECT created_at FROM center.affiliations WHERE operation_id=$1",&[&operation_id]).await.expect("independent effective time");
                assert_eq!(affiliation.created_at(),row.get::<_,chrono::DateTime<chrono::Utc>>(0));
                assert_eq!(observer.client().query_one(snapshot,&[&operation_id]).await.expect("retained exact facts").get::<_,String>(0),before);
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded permanent ownership");
        }));
    }

    #[test]
    fn history_pages_fixed_operator_identities_without_mutating_retained_values() {
        use crate::center::affiliation::{self, Code, Id};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30),Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context,services) = start(database.connections(&database.user),&supplier).await;
                let administrator = actor(&database,&services,&mut supplier).await;
                let member = register(&database,&services,&mut supplier).await;
                let person = services.current().read(&member).await.expect("target person");
                let original = operation();
                services.affiliation().create(member,original,serde_json::from_value(json!({"affiliation":"01"})).expect("first fill")).await.expect("actual first fill");
                let owner = database.connections("postgres").recover().await.expect("isolated fixture owner");
                let created_at: chrono::DateTime<chrono::Utc> = owner.client().query_one("SELECT created_at FROM center.affiliations WHERE operation_id=$1",&[&original]).await.expect("original effective time").get(0);
                // Retained material isolates pagination from the separately tested write protocol.
                let records = (1..=100).map(|offset| {
                    let id = if offset == 1 { Id(Uuid::from_u128(u128::MAX)) } else { Id::allocate() };
                    affiliation::database::Data { id, code: Code::Unaffiliated, operator_id: person.revision_id(), operator_person: person.person_id(), created_at: created_at-chrono::Duration::microseconds(offset), operation_id: operation() }
                }).collect();
                affiliation::tests::seed_history(owner.client(), services.affiliation(), person.person_id(), records).await;
                let snapshot = "SELECT jsonb_agg(to_jsonb(affiliation) ORDER BY created_at)::text FROM center.affiliations affiliation";
                let before: String = owner.client().query_one(snapshot,&[]).await.expect("independent original bytes").get(0);
                for (page,size,length) in [(1,1,1),(2,1,1),(1,20,20),(1,100,100),(2,100,1),(3,100,0),(i64::MAX,1,0)] {
                    let list = services.affiliation().history_by_person(&administrator,person.person_id(),Pagination::new(page,size).expect("page")).await.expect("qualified complete page");
                    assert_eq!(list.count(),101); assert_eq!(list.affiliations().len(),length);
                    for (index,affiliation) in list.affiliations().iter().enumerate() {
                        let offset = (page-1)*size+i64::try_from(index).expect("page index");
                        assert_eq!(affiliation.created_at(),created_at-chrono::Duration::microseconds(offset));
                        assert_eq!(affiliation.value(),if offset==0 { Code::CommunistParty } else { Code::Unaffiliated });
                        assert_eq!(affiliation.operator().person_id(),person.person_id());
                        assert_eq!(affiliation.operator().revision_id(),person.revision_id());
                        assert_eq!(affiliation.operator().name(),"T*");
                    }
                }
                assert_eq!(owner.client().query_one(snapshot,&[]).await.expect("read-only retained facts").get::<_,String>(0),before);
                owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded history pagination");
        }));
    }

    #[test]
    fn history_uses_fixed_facts_and_rejects_incomplete_pages() {
        use crate::center::affiliation::{self, Code, Id};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["value","missing_operator","name","id_number","person","empty_page","current"] {
                timeout(Duration::from_secs(30),Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context,services) = start(database.connections(&database.user),&supplier).await;
                    let administrator = actor(&database,&services,&mut supplier).await;
                    let member = register(&database,&services,&mut supplier).await;
                    let original = services.current().read(&member).await.expect("original person");
                    let operation_id = operation();
                    services.affiliation().create(member.clone(),operation_id,serde_json::from_value(json!({"affiliation":"01"})).expect("fill")).await.expect("actual first fill");
                    Box::pin(services.correction().correct(administrator.clone(),operation(),serde_json::from_value(json!({"target":{"personId":original.person_id(),"revisionId":original.revision_id()},"name":"Changed Person","idNumber":"00000020000229007X"})).expect("correction"))).await.expect("actual later identity");
                    let revised = services.current().read(&member).await.expect("current revision");
                    let owner = database.connections("postgres").recover().await.expect("isolated damage owner");
                    let created_at: chrono::DateTime<chrono::Utc> = owner.client().query_one("SELECT created_at FROM center.affiliations WHERE operation_id=$1",&[&operation_id]).await.expect("original time").get(0);
                    let later_at: chrono::DateTime<chrono::Utc> = owner.client().query_one("SELECT clock_timestamp()",&[]).await.expect("later retained fixture time").get(0);
                    assert!(later_at>created_at);
                    affiliation::tests::seed_history(owner.client(), services.affiliation(), original.person_id(), vec![affiliation::database::Data { id: Id::allocate(), code: Code::Unaffiliated, operator_id: revised.revision_id(), operator_person: revised.person_id(), created_at: later_at, operation_id: operation() }]).await;
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated retained fixture");
                    match case {
                        "value" => { owner.client().execute("UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38) # 1) WHERE operation_id=$1",&[&operation_id]).await.expect("damage older value"); }
                        "missing_operator" => { owner.client().execute("UPDATE center.affiliations SET operator_id=gen_random_uuid() WHERE operation_id=$1",&[&operation_id]).await.expect("missing original operator"); }
                        "name"|"id_number" => {
                            let sql = if case=="name" { "UPDATE identity.revisions SET name=set_byte(name,38,get_byte(name,38) # 1) WHERE id=$1" } else { "UPDATE identity.revisions SET id_number=set_byte(id_number,38,get_byte(id_number,38) # 1) WHERE id=$1" };
                            owner.client().execute(sql,&[&original.revision_id()]).await.expect("damage only historical identity");
                        }
                        "person"|"empty_page" => { owner.client().execute("DELETE FROM identity.persons WHERE id=$1",&[&original.person_id()]).await.expect("missing parent with retained values"); }
                        "current" => { owner.client().execute("UPDATE identity.persons SET revision_id=gen_random_uuid() WHERE id=$1",&[&original.person_id()]).await.expect("unrelated current pointer damage"); }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("fixture committed");
                    let snapshot = "SELECT jsonb_agg(to_jsonb(affiliation) ORDER BY created_at)::text FROM center.affiliations affiliation";
                    let before: String = owner.client().query_one(snapshot,&[]).await.expect("independent retained bytes").get(0);
                    if matches!(case,"person"|"empty_page") {
                        let row = owner.client().query_one("SELECT (SELECT count(*) FROM center.affiliations WHERE person_id=$1),EXISTS(SELECT 1 FROM identity.persons WHERE id=$1)",&[&original.person_id()]).await.expect("independent orphan observation");
                        assert_eq!(row.get::<_,i64>(0),2); assert!(!row.get::<_,bool>(1));
                    }
                    let result = services.affiliation().history_by_person(&administrator,original.person_id(),Pagination::new(if case=="empty_page" { 9 } else { 1 },20).expect("page")).await;
                    if case=="current" {
                        let list = result.expect("history does not load the current target profile");
                        assert_eq!(list.count(),2); assert_eq!(list.affiliations().len(),2);
                        assert_eq!(list.affiliations()[0].operator().name(),"C*"); assert_eq!(list.affiliations()[1].operator().name(),"T*");
                    } else {
                        let error = result.err().expect("no partial page delivered");
                        let Error::Corrupt(fault) = &error else { panic!("{case}: {error:?}"); };
                        if matches!(case,"name"|"id_number") { assert!(matches!(fault.kind(), FaultKind::Revision(_)), "{case}: {error:?}"); }
                        if case == "missing_operator" { assert!(matches!(fault.kind(), FaultKind::History(history::Error::Seal(crate::keys::seal::Error::Mismatch))), "{case}: {error:?}"); }
                        for private in ["13800000000","Test Person","Changed Person","SELECT ","UPDATE "] { assert!(!format!("{error:?}").contains(private)); }
                    }
                    assert_eq!(services.ready().await.is_ok(),case=="current");
                    assert_eq!(owner.client().query_one(snapshot,&[]).await.expect("read preserves retained facts").get::<_,String>(0),before);
                    owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error|panic!("history facts {case}: {error}"));
            }
        }));
    }

    async fn observe(
        service: &affiliation::Service,
        actor: &Actor,
        id: person::Id,
        route: &str,
    ) -> Result<serde_json::Value, Error> {
        let candidate = match route {
            "history" => {
                let list = service
                    .history_by_person(actor, id, Pagination::new(1, 20).expect("page"))
                    .await?;
                return Ok(
                    json!({"count":list.count(),"affiliations":list.affiliations().iter().map(|affiliation|json!({"value":affiliation.value(),"createdAt":affiliation.created_at(),"operator":{"personId":affiliation.operator().person_id(),"revisionId":affiliation.operator().revision_id(),"name":affiliation.operator().name()}})).collect::<Vec<_>>()}),
                );
            }
            "value" => {
                return Ok(serde_json::to_value(service.read_person(actor, id).await?)
                    .expect("value projection"));
            }
            "person" => service.candidate_by_person(actor, id).await?,
            "mobile" => {
                service
                    .candidate_by_mobile(actor, "13800000000".parse().expect("mobile"))
                    .await?
            }
            _ => unreachable!(),
        };
        Ok(
            json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"name":candidate.identity().name()}),
        )
    }

    fn fault(route: &str) -> fixture::database_proxy::Fault {
        use fixture::database_proxy::Fault;
        match route {
            "value" => Fault::StalledAffiliationRead,
            "history" => Fault::StalledAffiliationCount,
            "person" => Fault::StalledPerson,
            "mobile" => Fault::StalledAccountIdentity,
            _ => unreachable!(),
        }
    }

    #[test]
    fn management_keeps_permission_and_target_in_one_snapshot() {
        use fixture::database_proxy::Proxy;

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for route in ["value","person","mobile","history"] {
                timeout(Duration::from_secs(30),Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context,services) = crate::access::role::tests::start_snapshot(authentication::proxied(&database,&proxy),&supplier).await;
                    let administrator = actor(&database,&services,&mut supplier).await;
                    let manager = Box::pin(register_manager(&database,&services,&mut supplier)).await;
                    let member = register(&database,&services,&mut supplier).await;
                    let target = services.current().read(&member).await.expect("original identity");
                    let mut role = create_role(&services,&administrator,"Reader",20,&["institution.affiliations.read"]).await;
                    Box::pin(assign(&services,&administrator,"13700000000",&role)).await;
                    proxy.arm(fault(route));
                    let service = services.affiliation().clone(); let reader_actor = manager.clone(); let id = target.person_id();
                    let reader = tokio::spawn(async move { observe(&service,&reader_actor,id,route).await });
                    timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("target query follows current permission in the snapshot");
                    role = Box::pin(services.role().update(administrator.clone(),operation(),role.id(),serde_json::from_value(json!({"changeId":role.change_id(),"name":"Reader","level":20,"permissions":[]})).expect("withdraw permission"))).await.expect("actual permission loss");
                    Box::pin(services.correction().correct(administrator.clone(),operation(),serde_json::from_value(json!({"target":{"personId":target.person_id(),"revisionId":target.revision_id()},"name":"Changed Person","idNumber":"00000020000229007X"})).expect("correction"))).await.expect("actual concurrent correction");
                    services.affiliation().create(member.clone(),operation(),serde_json::from_value(json!({"affiliation":"01"})).expect("fill")).await.expect("actual concurrent first fill");
                    proxy.release();
                    let result = timeout(Duration::from_secs(2),reader).await.expect("snapshot finished").expect("reader task");
                    let revised = services.current().read(&member).await.expect("committed current revision");
                    assert_ne!(revised.revision_id(),target.revision_id());
                    assert!(matches!(result,Err(Error::Denied)), "the moved state anchor retries authorization in a fresh snapshot: {route}: {result:?}");
                    assert_eq!(observe(services.affiliation(),&manager,target.person_id(),route).await.expect_err("current permission lost").code(),Some("permission_denied"));
                    Box::pin(services.role().update(administrator,operation(),role.id(),serde_json::from_value(json!({"changeId":role.change_id(),"name":"Reader","level":20,"permissions":["institution.affiliations.read"]})).expect("restore permission"))).await.expect("actual new permission");
                    let value = observe(services.affiliation(),&manager,target.person_id(),route).await.expect("new snapshot");
                    if route == "value" { assert_eq!(value["value"],"01"); }
                    else if route == "history" { assert_eq!(value["count"],1); assert_eq!(value["affiliations"][0]["value"],"01"); assert_eq!(value["affiliations"][0]["operator"],json!({"personId":revised.person_id(),"revisionId":revised.revision_id(),"name":"C*"})); }
                    else { assert_eq!(value,json!({"personId":revised.person_id(),"revisionId":revised.revision_id(),"name":"C*"})); }
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error|panic!("snapshot route {route}: {error}"));
            }
        }));
    }

    #[test]
    fn management_read_failures_end_connections_and_withhold_results() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (route,case) in [("value","cancel"),("person","cancel"),("mobile","cancel"),("value","timeout"),("value","commit"),("person","query"),("mobile","query"),("history","cancel"),("history","timeout"),("history","commit"),("history","query")] {
                timeout(Duration::from_secs(25),Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context,services) = start(authentication::proxied(&database,&proxy),&supplier).await;
                    let administrator = actor(&database,&services,&mut supplier).await;
                    let member = register(&database,&services,&mut supplier).await;
                    let target = services.current().read(&member).await.expect("target");
                    services.affiliation().create(member,operation(),serde_json::from_value(json!({"affiliation":"01"})).expect("fill")).await.expect("first fill");
                    let owner = database.connections("postgres").recover().await.expect("independent observer");
                    let snapshot = "SELECT to_jsonb(affiliation)::text FROM center.affiliations affiliation";
                    let before: String = owner.client().query_one(snapshot,&[]).await.expect("original bytes").get(0);
                    proxy.arm(fault(route));
                    let service = services.affiliation().clone(); let reader_actor = administrator.clone(); let id = target.person_id();
                    let reader = tokio::spawn(async move { observe(&service,&reader_actor,id,route).await });
                    let pid = timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("actual target query intercepted");
                    match case {
                        "query" => owner.client().batch_execute(&format!("REVOKE SELECT ON identity.revisions FROM {}",database.user)).await.expect("isolated target query denial"),
                        "commit" => proxy.arm(Fault::LostReadCommit),
                        _ => {},
                    }
                    if !matches!(case,"cancel"|"timeout") { proxy.release(); }
                    if case=="cancel" {
                        reader.abort(); assert!(reader.await.expect_err("reader cancelled").is_cancelled());
                    } else {
                        let error = timeout(Duration::from_secs(4),reader).await.expect("bounded failure").expect("reader task").expect_err("no result delivered");
                        let Error::Unavailable(fault) = &error else { panic!("{route}/{case}: {error:?}"); };
                        match case {
                            "timeout" => assert!(matches!(fault.kind(), FaultKind::Timeout(_)), "{error:?}"),
                            "commit" => assert!(matches!(fault.kind(), FaultKind::Query(_)) && fault.stage() == Stage::Read, "{error:?}"),
                            "query" => assert!(matches!(fault.kind(), FaultKind::Person(_) | FaultKind::Account(_) | FaultKind::Query(_) | FaultKind::History(history::Error::Query(_)) | FaultKind::Revision(crate::identity::person::revision::ReadError::Query(_))), "{error:?}"),
                            _ => unreachable!(),
                        }
                    }
                    proxy.release();
                        timeout(Duration::from_secs(3),async { while owner.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1)",&[&pid]).await.expect("original backend observation").get::<_,bool>(0) { tokio::task::yield_now().await; } }).await.expect("original backend ends before fixture cleanup");
                    if case == "query" { owner.client().batch_execute(&format!("GRANT SELECT ON identity.revisions TO {}",database.user)).await.expect("restore isolated query privilege"); }
                    services.ready().await.expect("ordinary failure preserves admission");
                    assert!(observe(services.affiliation(),&administrator,target.person_id(),route).await.is_ok());
                    assert_eq!(owner.client().query_one(snapshot,&[]).await.expect("read-only facts").get::<_,String>(0),before);
                    services.close(); services.drain().await.expect("drained"); let _ = context.stop().await;
                    timeout(Duration::from_secs(3),async { while owner.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND usename=$1)",&[&database.user]).await.expect("all runtime backends").get::<_,bool>(0) { tokio::task::yield_now().await; } }).await.expect("runtime termination before fixture cleanup");
                    owner.terminate().await.expect("observer ended"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error|panic!("read failure {route}/{case}: {error}"));
            }
        }));
    }

    #[test]
    fn candidates_do_not_read_affiliations_and_integrity_failures_close_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["query", "value", "person", "revision", "hash", "number", "name"] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let administrator = actor(&database,&services,&mut supplier).await;
                    let member = register(&database,&services,&mut supplier).await;
                    let target = services.current().read(&member).await.expect("target");
                    let manager = Box::pin(register_manager(&database,&services,&mut supplier)).await;
                    let role = create_role(&services,&administrator,"Correction Only",20,&["institution.affiliations.correct"]).await;
                    Box::pin(assign(&services,&administrator,"13700000000",&role)).await;
                    let before = services.affiliation().candidate_by_person(&manager,target.person_id()).await.expect("unfilled target");
                    services.affiliation().create(member,operation(),serde_json::from_value(json!({"affiliation":"01"})).expect("fill")).await.expect("first fill");
                    let after = services.affiliation().candidate_by_person(&manager,target.person_id()).await.expect("filled target");
                    assert_eq!(before.target(),after.target()); assert_eq!(before.identity().name(),after.identity().name()); assert_eq!(before.identity().revision_id(),after.identity().revision_id());
                    let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                    match case {
                        "query" => owner.client().batch_execute(&format!("REVOKE SELECT ON center.affiliations FROM {}",database.user)).await.expect("deny isolated value query"),
                        "value" => owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38) # 1); COMMIT").await.expect("damage only retained value"),
                        _ => {
                            owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated identity damage");
                            let sql = match case {
                                "person" => "DELETE FROM identity.persons WHERE id=$1",
                                "revision" => "UPDATE identity.persons SET revision_id=$2 WHERE id=$1",
                                "hash" => "UPDATE identity.accounts SET id_number=set_byte(id_number,0,get_byte(id_number,0) # 1) WHERE person_id=$1",
                                "number" => "UPDATE identity.revisions SET id_number=set_byte(id_number,38,get_byte(id_number,38) # 1) WHERE person_id=$1",
                                "name" => "UPDATE identity.revisions SET name=set_byte(name,38,get_byte(name,38) # 1) WHERE person_id=$1",
                                _ => unreachable!(),
                            };
                            if case == "revision" { owner.client().execute(sql,&[&target.person_id(),&services.current().read(&administrator).await.expect("other identity").revision_id()]).await.expect("wrong current revision"); }
                            else { owner.client().execute(sql,&[&target.person_id()]).await.expect("damaged identity"); }
                            owner.client().batch_execute("COMMIT").await.expect("fixture committed");
                        }
                    }
                    if case == "person" {
                        let row = owner.client().query_one("SELECT EXISTS(SELECT 1 FROM center.affiliations WHERE person_id=$1), EXISTS(SELECT 1 FROM identity.persons WHERE id=$1)", &[&target.person_id()]).await.expect("retained value and missing parent before reading");
                        assert!(row.get::<_,bool>(0)); assert!(!row.get::<_,bool>(1));
                    }
                    if matches!(case,"query"|"value") {
                        for result in [services.affiliation().candidate_by_person(&manager,target.person_id()).await,services.affiliation().candidate_by_mobile(&manager,"13800000000".parse().expect("mobile")).await] {
                            let candidate = result.expect("candidate never depends on value or presence"); assert_eq!(candidate.identity().revision_id(),target.revision_id());
                        }
                        assert_eq!(services.affiliation().read_person(&manager,target.person_id()).await.err().expect("correction cannot read").code(),Some("permission_denied"));
                    }
                    let error = if matches!(case,"query"|"value"|"person") { services.affiliation().read_person(&administrator,target.person_id()).await.err().expect("read fails safely") }
                        else { services.affiliation().candidate_by_person(&administrator,target.person_id()).await.err().expect("invalid current identity") };
                    if case == "query" {
                        assert!(matches!(&error, Error::Unavailable(fault) if matches!(fault.kind(), FaultKind::History(history::Error::Query(_)))), "{case}: {error:?}");
                    } else {
                        assert!(matches!(&error, Error::Corrupt(_)), "{case}: {error:?}");
                    }
                    assert_eq!(services.ready().await.is_err(),case!="query");
                    if case == "query" {
                        owner.client().batch_execute(&format!("GRANT SELECT ON center.affiliations TO {}",database.user)).await.expect("restore isolated privilege");
                        assert!(services.affiliation().read_person(&administrator,target.person_id()).await.is_ok());
                    }
                    owner.terminate().await.expect("owner ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error|panic!("integrity case {case}: {error}"));
            }
        }));
    }
}
