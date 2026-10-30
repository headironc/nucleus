//! Account use cases combine current Identity facts and any required Access qualification.

mod avatar;
pub mod transfer;

use std::sync::Arc;

use tokio::time::{Instant, timeout_at};

use crate::{
    access::{self, appointment, role},
    database::{
        Query,
        transaction::{self, EndError},
    },
    identity::{
        self, account,
        current::{self, Actor},
        keys::Keys,
        mobile::Mobile,
        suspension,
    },
    kernel::App,
    runtime::{
        Context,
        registry::Registry,
        work::{self, Failure as _},
    },
};

pub struct Candidate {
    identity: account::Subject,
    suspension_id: Option<suspension::Id>,
}

impl Candidate {
    pub fn identity(&self) -> &account::Subject {
        &self.identity
    }
    pub fn suspension_id(&self) -> Option<suspension::Id> {
        self.suspension_id
    }
}

#[derive(Clone)]
pub struct Service {
    context: Context,
    current: current::Service,
    roles: role::Reader,
    keys: Arc<Keys>,
    accounts: account::Reader,
    avatar: identity::avatar::Service,
    registry: Arc<Registry>,
    work: Arc<work::Work>,
}

impl Service {
    pub(crate) fn prepare(
        app: App,
        current: current::Service,
        keys: Arc<Keys>,
        avatar: identity::avatar::Service,
    ) -> Self {
        Self {
            context: app.context().clone(),
            current,
            registry: Arc::clone(app.registry()),
            work: Arc::clone(app.work()),
            roles: role::Reader::new(app.clone()),
            accounts: account::Reader::new(app, Arc::clone(&keys)),
            keys,
            avatar,
        }
    }

    pub async fn candidate(&self, actor: &Actor, mobile: Mobile) -> Result<Candidate, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    let mobile = &mobile;
                    async move {
                        let (role, person_id, _, now) = access::authorize(
                            client,
                            &roles,
                            &self.current,
                            actor,
                            role::Code::InstitutionAccounts,
                        )
                        .await?;
                        let account_id = self
                            .accounts
                            .at(view)
                            .find_by_mobile(client, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        let target = appointment::find_by_account(client, &roles, account_id, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        if target.person_id() == person_id || target.role().level() >= role.level()
                        {
                            return Err(Error::Absent);
                        }
                        let identity = self
                            .accounts
                            .at(view)
                            .find_by_id(client, account_id, mobile, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        let suspension_id = identity.suspension_id();
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

    pub async fn fetch_transfer(
        &self,
        actor: &Actor,
        id: identity::transfer::Id,
    ) -> Result<identity::transfer::State, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| async move {
                    let roles = self.roles.at(view);
                    let (person_id, _, now) =
                        self.current.at(view).fetch_person(client, actor).await?;
                    let snapshot = identity::transfer::find_by_id(client, roles.keys(), id, now)
                        .await?
                        .ok_or(Error::Absent)?;
                    if snapshot.person_id() != person_id {
                        let manager = appointment::find_by_account(
                            client,
                            &roles,
                            actor.account_id(),
                            now,
                        )
                        .await?
                        .ok_or(appointment::Error::Facts {
                            rule: "the confirmed actor retains an account in the read snapshot",
                        })?;
                        if !manager
                            .role()
                            .permissions()
                            .contains(&role::Code::InstitutionAccounts)
                        {
                            return Err(Error::Absent);
                        }
                        let account_id = self
                            .accounts
                            .at(view)
                            .find_by_person(client, snapshot.person_id(), now)
                            .await?
                            .ok_or(Error::Absent)?;
                        let target = appointment::find_by_account(client, &roles, account_id, now)
                            .await?
                            .ok_or(Error::Absent)?;
                        if target.role().level() >= manager.role().level() {
                            return Err(Error::Absent);
                        }
                    }
                    Ok(snapshot.into_state(now)?)
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    async fn finish<T>(
        &self,
        lease: platform::database::Lease,
        deadline: Instant,
        result: Result<T, Error>,
    ) -> Result<T, Error> {
        if let Err(error) = &result
            && error.unreliable()
        {
            self.registry.close();
        }
        match result {
            Ok(value) => {
                let (known, cleanup) =
                    transaction::confirm(value, deadline, lease.client().batch_execute("COMMIT"))
                        .await
                        .recycle(lease)
                        .await;
                match known.end {
                    Ok(()) => cleanup.map_err(Error::from).map(|()| known.value),
                    Err(error) => Err(Error::End(error).combine(cleanup)),
                }
            }
            Err(error) if error.failure() != Failure::Unavailable => {
                let (known, cleanup) = transaction::rollback(
                    error,
                    deadline,
                    lease.client().batch_execute("ROLLBACK"),
                )
                .await
                .recycle(lease)
                .await;
                Err(match known.end {
                    Ok(()) => known.value,
                    Err(rollback) => Error::Rollback {
                        cause: Box::new(known.value),
                        rollback,
                    },
                }
                .combine(cleanup))
            }
            Err(error) => Err(error.combine(lease.discard().await)),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::expect_used)]
    use std::{sync::Arc, time::Duration};

    use tokio::time::timeout;
    use uuid::Uuid;

    use crate::{
        access::{
            appointment,
            role::tests::{actor, start},
        },
        identity::authentication::{Database, Supplier, server},
    };

    fn operation() -> crate::identity::operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    pub(crate) async fn register_manager(
        database: &Database,
        services: &crate::startup::Services,
        supplier: &mut Supplier,
    ) -> crate::identity::current::Actor {
        use crate::identity::authentication::session::{
            Receipt,
            tests::{CODE, input, message},
        };
        let id = message(database, services.authentication(), supplier, "13700000000").await;
        let Receipt::Registration(credential) = services
            .authentication()
            .create_session(input(Uuid::new_v4(), id, "13700000000", CODE))
            .await
            .expect("R1")
        else {
            panic!("registration");
        };
        let issued = services
            .registration()
            .register(crate::registration::Input::new(
                operation(),
                credential.id(),
                credential.token().text().parse().expect("proof"),
                "Manager".parse().expect("name"),
                "000000200002290029".parse().expect("number"),
            ))
            .await
            .expect("R2");
        services
            .current()
            .authenticate(crate::identity::current::Proof {
                id: issued.id(),
                token: issued.token().text().parse().expect("token"),
            })
            .await
            .expect("manager actor")
    }

    pub(crate) async fn assign(
        services: &crate::startup::Services,
        administrator: &crate::identity::current::Actor,
        mobile: &str,
        role: &crate::access::role::Role,
    ) {
        let candidate = services
            .appointment()
            .candidate(administrator, mobile.parse().expect("mobile"))
            .await
            .expect("candidate");
        let input = serde_json::from_value(serde_json::json!({"target":{"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":mobile,"assignmentId":candidate.assignment_id()},"role":{"id":role.id(),"changeId":role.change_id()}})).expect("assignment");
        services
            .appointment()
            .assign(administrator.clone(), operation(), input)
            .await
            .expect("real assignment");
    }

    async fn establish(
        database: &Database,
        services: &crate::startup::Services,
        target: &crate::identity::current::Actor,
        supplier: &mut Supplier,
        sms: &mut fixture::supplier::Supplier,
    ) -> crate::identity::transfer::Transfer {
        crate::identity::authentication::session::tests::age(
            database,
            services.authentication(),
            chrono::TimeDelta::seconds(61),
        )
        .await;
        let (_, sent, code) = crate::identity::transfer::tests::send_to(
            database,
            services,
            target,
            supplier,
            sms,
            "13800000000",
        )
        .await;
        services
            .transfer()
            .create(
                target.clone(),
                crate::identity::transfer::Input::new(
                    operation(),
                    sent.id(),
                    code.parse().expect("code"),
                ),
            )
            .await
            .expect("actual self transfer")
    }

    #[test]
    fn mobile_status_uses_current_scope_and_one_snapshot_during_permission_changes() {
        use crate::{access::role, identity::transfer};
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = transfer::tests::start(&database, &supplier, &sms).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let target = appointment::tests::register(&database, &services, &mut supplier).await;
                let manager = register_manager(&database, &services, &mut supplier).await;
                let transfer = establish(&database, &services, &target, &mut supplier, &mut sms).await;
                let pending = transfer::State::Pending { expires_at: transfer.expires_at() };
                let work = Box::pin(async {
                    assert_eq!(services.account().fetch_transfer(&target, transfer.id()).await.expect("self"), pending);
                    assert_eq!(services.account().fetch_transfer(&administrator, transfer.id()).await.expect("administrator"), pending);
                    assert!(matches!(services.account().fetch_transfer(&manager, transfer.id()).await, Err(super::Error::Absent)));
                    let role_input = |name: &str, level: i16, permissions: &[&str]| serde_json::from_value(serde_json::json!({"name":name,"level":level,"permissions":permissions})).expect("role definition");
                    let target_role = services.role().create(administrator.clone(), operation(), role_input("Target", 5, &[])).await.expect("target role");
                    let mut managed = services.role().create(administrator.clone(), operation(), role_input("Manager", 10, &["institution.accounts"])).await.expect("manager role");
                    assign(&services, &administrator, "13800000000", &target_role).await;
                    assign(&services, &administrator, "13700000000", &managed).await;
                    for (level, permissions, allowed) in [(10, vec!["institution.accounts"], true), (5, vec!["institution.accounts"], false), (4, vec!["institution.accounts"], false), (10, vec!["institution.roles"], false), (10, vec!["institution.accounts"], true)] {
                        if i16::from(managed.level()) != level || managed.permissions().iter().map(|code| code.as_ref()).collect::<Vec<_>>() != permissions {
                            managed = services.role().update(administrator.clone(), operation(), managed.id(), role::Update::new(managed.change_id(), role_input("Manager", level, &permissions))).await.expect("actual role change");
                        }
                        let result = services.account().fetch_transfer(&manager, transfer.id()).await;
                        if allowed { assert_eq!(result.expect("current higher manager"), pending); }
                        else { assert!(matches!(result, Err(super::Error::Absent)), "{result:?}"); }
                        assert_eq!(services.account().fetch_transfer(&target, transfer.id()).await.expect("self requires no management power"), pending);
                    }
                    let gate = database.connections("postgres").recover().await.expect("isolated gate");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let blocker: i32 = gate.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("backend").get(0);
                    gate.client().batch_execute("BEGIN; LOCK TABLE identity.transfers IN ACCESS EXCLUSIVE MODE").await.expect("pause result query after identity snapshot");
                    let reader = services.account().clone(); let current = manager.clone(); let id = transfer.id();
                    let task = tokio::spawn(async move { reader.fetch_transfer(&current, id).await });
                    timeout(Duration::from_secs(2), async { loop {
                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND wait_event_type='Lock')", &[&blocker]).await.expect("actual result-query waiter").get::<_,bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("read holds its decisive snapshot");
                    managed = services.role().update(administrator.clone(), operation(), managed.id(), role::Update::new(managed.change_id(), role_input("Manager", 10, &[]))).await.expect("revoke permission after read snapshot");
                    gate.client().batch_execute("ROLLBACK").await.expect("release query");
                    // The old role state disagrees with its advanced anchor, so the whole read retries.
                    let result = task.await.expect("reader");
                    assert!(matches!(result, Err(super::Error::Absent)), "{result:?}");
                    assert!(matches!(services.account().fetch_transfer(&manager, id).await, Err(super::Error::Absent)));
                    let unknown = serde_json::from_value(serde_json::json!(Uuid::new_v4())).expect("unknown transfer ID");
                    assert!(matches!(services.account().fetch_transfer(&manager, unknown).await, Err(super::Error::Absent)));
                    assert_eq!(services.account().fetch_transfer(&target, id).await.expect("self remains readable after the complete retry"), pending);
                    services.role().update(administrator.clone(), operation(), managed.id(), role::Update::new(managed.change_id(), role_input("Manager", 10, &["institution.accounts"]))).await.expect("restore permission through the sealed role definition");
                    assert_eq!(services.account().fetch_transfer(&manager, id).await.expect("current restored permission"), pending);
                    assert!(observer.client().query_one("SELECT (SELECT count(*) FROM identity.transfers)=1 AND NOT EXISTS(SELECT 1 FROM identity.replacements) AND NOT EXISTS(SELECT 1 FROM identity.rejections) AND NOT EXISTS(SELECT 1 FROM identity.revocations)", &[]).await.expect("reads create no business consequences").get::<_,bool>(0));
                    observer.terminate().await.expect("observer ended"); gate.terminate().await.expect("gate ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("context ended");
                });
                tokio::select! { biased; _ = supplier.observe() => panic!("status and role reads never verify"), _ = sms.request() => panic!("status and role reads never send SMS"), () = work => {} }
                sms.stop().await; supplier.stop().await;
            })).await.expect("bounded status qualification and snapshot checks");
        }));
    }

    #[test]
    fn mobile_status_preserves_paused_targets_completion_cleanup_and_cancellation() {
        use crate::identity::transfer;
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for completed in [false, true] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await; let mut sms = fixture::supplier::Supplier::start().await;
                    let (context, services) = transfer::tests::start(&database, &supplier, &sms).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = appointment::tests::register(&database, &services, &mut supplier).await;
                    let transfer = establish(&database, &services, &target, &mut supplier, &mut sms).await;
                    let mobile = if completed {
                        let (_, sent, code) = transfer::tests::bind_to(&database, &services, &target, transfer.id(), &mut supplier, &mut sms, "13600000000").await;
                        services.transfer().complete(target.clone(), transfer::completion::Input::new(operation(), "13600000000".parse().expect("new mobile"), sent.id(), code.parse().expect("code"))).await.expect("actual completion");
                        "13600000000"
                    } else { "13800000000" };
                    let work = Box::pin(async {
                        let expected = services.account().fetch_transfer(&manager, transfer.id()).await.expect("authorized original status");
                        assert_eq!(matches!(expected, transfer::State::Completed { .. }), completed);
                        let candidate = services.account().candidate(&manager, mobile.parse().expect("mobile")).await.expect("current target");
                        let input = serde_json::json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":mobile});
                        services.suspension().suspend(manager.clone(), operation(), candidate.identity().id(), serde_json::from_value(input).expect("suspension input")).await.expect("actual target suspension");
                        assert_eq!(services.account().fetch_transfer(&manager, transfer.id()).await.expect("paused target remains readable"), expected);
                        assert_eq!(services.account().fetch_transfer(&target, transfer.id()).await.expect_err("paused or revoked actor").failure(), super::Failure::Invalid);
                        if !completed {
                            crate::registration::tests::age_facts(&database, &services, chrono::TimeDelta::minutes(11)).await;
                            assert_eq!(services.account().fetch_transfer(&manager, transfer.id()).await.expect("expired transfer"), transfer::State::Expired);
                        }
                    });
                    tokio::select! { biased; _ = supplier.observe() => panic!("status and suspension never verify"), _ = sms.request() => panic!("status and suspension never send SMS"), () = work => {} }
                    let manager = if completed {
                        crate::registration::tests::age_facts(&database, &services, chrono::TimeDelta::hours(26)).await;
                        for _ in 0..3 {
                            services.transfer().retention().run().await.expect("transfer retention");
                            services.authentication().retention().run().await.expect("message retention");
                        }
                        actor(&database, &services, &mut supplier).await
                    } else { manager };
                    let work = Box::pin(async {
                        let observer = database.connections(&database.user).recover().await.expect("independent observer");
                        if completed {
                            assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.transfers WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.replacements WHERE id=$1)", &[&transfer.id()]).await.expect("retained completion without short transfer").get::<_,bool>(0));
                            assert!(matches!(services.account().fetch_transfer(&manager, transfer.id()).await.expect("managed retained completion"), transfer::State::Completed { .. }));
                        }
                        let candidate = services.account().candidate(&manager, mobile.parse().expect("mobile")).await.expect("paused target confirmation");
                        let input = serde_json::from_value(serde_json::json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":mobile})).expect("cancellation confirmation");
                        let cancelled=services.cancellation().cancel_account(manager.clone(), operation(), candidate.identity().id(), input).await.expect("actual target cancellation");
                        let facts=observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.accounts WHERE id=$1 AND person_id=$3 AND cancellation_id=$4 AND mobile IS NULL AND id_number IS NULL AND suspension_id IS NULL) AS account,(SELECT count(*) FROM identity.replacements WHERE id=$2 AND account_id=$1 AND person_id=$3) AS replacements,(SELECT count(*) FROM identity.transfers WHERE id=$2 AND person_id=$3) AS transfers", &[&target.account_id(),&transfer.id(),&candidate.identity().person_id(),&cancelled.id()]).await.expect("permanent tombstone, fixed replacement and short transfer retention before read");
                        assert!(facts.get::<_,bool>("account"));
                        assert_eq!(facts.get::<_,i64>("replacements"),i64::from(completed));
                        assert_eq!(facts.get::<_,i64>("transfers"),i64::from(!completed));
                        assert!(matches!(services.account().fetch_transfer(&manager, transfer.id()).await, Err(super::Error::Absent)));
                        observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("context ended");
                    });
                    tokio::select! { biased; _ = supplier.observe() => panic!("retained read and cancellation never verify"), _ = sms.request() => panic!("retained read and cancellation never send SMS"), () = work => {} }
                    sms.stop().await; supplier.stop().await;
                })).await.expect("bounded managed status lifecycle");
            }
        }));
    }

    #[test]
    fn mobile_status_discards_interrupted_reads_and_closes_only_on_unreliable_facts() {
        use crate::identity::transfer;
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = transfer::tests::start(&database, &supplier, &sms).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let target = appointment::tests::register(&database, &services, &mut supplier).await;
                let transfer = establish(&database, &services, &target, &mut supplier, &mut sms).await;
                let work = Box::pin(async {
                    let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                    let observer = database.connections(&database.user).recover().await.expect("runtime observer");
                    let blocker: i32 = owner.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("actual lock owner").get(0);
                    for cancelled in [false, true] {
                        owner.client().batch_execute("BEGIN; LOCK TABLE identity.transfers IN ACCESS EXCLUSIVE MODE").await.expect("block result query");
                        let reader = services.account().clone();
                        let current = manager.clone();
                        let id = transfer.id();
                        let task = tokio::spawn(async move { reader.fetch_transfer(&current, id).await });
                        let backend: i32 = timeout(Duration::from_secs(2), async { loop {
                            if let Some(row) = observer.client().query_opt("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND wait_event_type='Lock'", &[&blocker]).await.expect("actual status query waiter") {
                                break row.get(0);
                            }
                            tokio::task::yield_now().await;
                        }}).await.expect("status query reached the gate");
                        if cancelled {
                            task.abort();
                            assert!(task.await.expect_err("cancelled read").is_cancelled());
                        } else {
                            let error = timeout(Duration::from_secs(4), task).await.expect("bounded status read").expect("reader").expect_err("unfinished query withheld");
                            assert_eq!(error.failure(), super::Failure::Unavailable);
                            assert!(!error.unreliable());
                        }
                        owner.client().batch_execute("ROLLBACK").await.expect("release query gate");
                        timeout(Duration::from_secs(3), async { loop {
                            if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&backend]).await.expect("reader backend ended").get::<_,bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }}).await.expect("actual connection discarded");
                        services.account().fetch_transfer(&manager, id).await.expect("ordinary interruption leaves admission open");
                    }
                    owner.client().batch_execute(&format!("REVOKE SELECT ON identity.transfers FROM {}", database.user)).await.expect("isolated ordinary query failure");
                    let error = services.account().fetch_transfer(&manager, transfer.id()).await.expect_err("query failure is not 404");
                    assert_eq!(error.failure(), super::Failure::Unavailable);
                    assert!(!error.unreliable());
                    assert!(services.account().registry.ready().is_ok());
                    owner.client().batch_execute(&format!("GRANT SELECT ON identity.transfers TO {}", database.user)).await.expect("restore disposable grant");
                    services.account().fetch_transfer(&manager, transfer.id()).await.expect("read after ordinary failure");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("explicit integrity fault");
                    owner.client().execute("UPDATE identity.transfers SET created_at=clock_timestamp()+interval '1 hour' WHERE id=$1", &[&transfer.id()]).await.expect("future result after startup");
                    owner.client().batch_execute("COMMIT").await.expect("fault committed");
                    let error = services.account().fetch_transfer(&manager, transfer.id()).await.expect_err("unreliable fact withheld");
                    assert_eq!(error.failure(), super::Failure::Unavailable);
                    assert!(error.unreliable());
                    assert!(services.account().registry.ready().is_err());
                    assert!(matches!(services.account().fetch_transfer(&manager, transfer.id()).await, Err(super::Error::Work(crate::runtime::work::Error::Closed))));
                    assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.replacements) AND NOT EXISTS(SELECT 1 FROM identity.revocations)", &[]).await.expect("failed reads write no outcome").get::<_,bool>(0));
                    observer.terminate().await.expect("observer ended");
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("context ended");
                });
                tokio::select! { biased; _ = supplier.observe() => panic!("failed status reads never verify"), _ = sms.request() => panic!("failed status reads never send SMS"), () = work => {} }
                sms.stop().await; supplier.stop().await;
            })).await.expect("bounded read failure checks");
        }));
    }

    fn input(candidate: &super::Candidate) -> crate::suspension::Input {
        serde_json::from_value(serde_json::json!({"personId":candidate.identity().person_id(), "revisionId":candidate.identity().revision_id(), "mobile":"13800000000"})).expect("fixed target confirmation")
    }

    #[test]
    fn candidate_reads_close_on_damaged_facts_but_not_query_failure() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for damaged in [false,true] {
                let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let manager=actor(&database,&services,&mut supplier).await;Box::pin(appointment::tests::register(&database,&services,&mut supplier)).await;
                let candidate=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");
                let operation=Uuid::new_v4().to_string().parse().expect("operation");
                Box::pin(services.suspension().suspend(manager.clone(),operation,candidate.identity().id(),input(&candidate))).await.expect("real suspension");
                let owner=database.connections("postgres").recover().await.expect("isolated owner");
                if damaged {
                    let before:Vec<u8>=owner.client().query_one("SELECT seal FROM identity.accounts WHERE id=$1",&[&candidate.identity().id()]).await.expect("original target seal").get(0);
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated target damage");
                    assert_eq!(owner.client().execute("UPDATE identity.accounts SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE id=$1",&[&candidate.identity().id()]).await.expect("damage target seal"),1);
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let after:Vec<u8>=owner.client().query_one("SELECT seal FROM identity.accounts WHERE id=$1",&[&candidate.identity().id()]).await.expect("changed target seal").get(0);
                    let mut expected=before;expected[0]^=1;assert_eq!(after,expected);
                }
                else {owner.client().batch_execute(&format!("REVOKE SELECT ON identity.accounts FROM {}",database.user)).await.expect("ordinary query denied");}
                let error=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.err().expect("no damaged or unread candidate disclosed");
                if damaged {
                    assert!(matches!(&error,super::Error::Account(crate::identity::account::ReadError::History(crate::history::Error::Seal(crate::keys::seal::Error::Mismatch)))),"{error:?}");
                } else {
                    assert!(matches!(&error,super::Error::Role(crate::access::role::Error::Current(crate::identity::current::Error::History(crate::history::Error::Query(query)))) if query.code()==Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)),"{error:?}");
                }
                assert_eq!(error.failure(),super::Failure::Unavailable);assert_eq!(error.unreliable(),damaged);
                assert_eq!(services.account().registry.ready().is_err(),damaged);
                if !damaged {owner.client().batch_execute(&format!("GRANT SELECT ON identity.accounts TO {}",database.user)).await.expect("restore disposable permissions");services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("ordinary failure leaves admission usable");}
                else {assert!(Box::pin(services.suspension().suspend(manager,operation,candidate.identity().id(),input(&candidate))).await.err().expect("closed admission").is_closed());}
                assert_eq!(owner.client().query_one("SELECT count(*) FROM identity.suspensions",&[]).await.expect("read failure writes nothing").get::<_,i64>(0),1);
                owner.terminate().await.expect("owner ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            }
        }));
    }

    #[test]
    fn candidate_deadline_and_cancellation_end_the_actual_read_connection() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(15),Box::pin(async {
                let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let manager=actor(&database,&services,&mut supplier).await;Box::pin(appointment::tests::register(&database,&services,&mut supplier)).await;
                let owner=database.connections("postgres").recover().await.expect("isolated lock owner");
                let observer=database.connections(&database.user).recover().await.expect("independent observer");
                let blocker:i32=owner.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("actual blocker").get(0);
                for cancelled in [false,true] {
                    owner.client().batch_execute("BEGIN; LOCK TABLE identity.accounts IN ACCESS EXCLUSIVE MODE").await.expect("block only this fixture's account read");
                    let service=services.account().clone();let actor=manager.clone();let started=std::time::Instant::now();
                    let task=tokio::spawn(async move {service.candidate(&actor,"13800000000".parse().expect("mobile")).await});
                    let backend:i32=timeout(Duration::from_secs(2),async {loop {if let Some(row)=observer.client().query_opt("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND wait_event_type='Lock'",&[&blocker]).await.expect("actual candidate read waits"){break row.get(0);}tokio::task::yield_now().await;}}).await.expect("read reached blocked statement");
                    if cancelled {task.abort();assert!(task.await.err().expect("caller cancelled").is_cancelled());}
                    else {
                        let error=timeout(Duration::from_secs(4),task).await.expect("shared three-second deadline").expect("read worker").err().expect("incomplete snapshot withheld");
                        assert_eq!(error.failure(),super::Failure::Unavailable);assert!(!error.unreliable(),"{error:?}");
                        assert!(started.elapsed()>=Duration::from_millis(2500) && started.elapsed()<Duration::from_millis(4500));
                    }
                    owner.client().batch_execute("ROLLBACK").await.expect("release read gate");
                    timeout(Duration::from_secs(3),async {loop {if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",&[&backend]).await.expect("driver backend ended").get::<_,bool>(0){break;}tokio::task::yield_now().await;}}).await.expect("cancelled or timed-out read connection discarded");
                    services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("clean connection and admission after ordinary read failure");
                }
                owner.terminate().await.expect("owner ended");observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("candidate read resource budget");
        }));
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the target is unavailable")]
    Absent,
    #[error("{0}")]
    Current(#[from] current::Error),
    #[error("{0}")]
    Role(#[from] role::Error),
    #[error("{0}")]
    Appointment(#[from] appointment::Error),
    #[error("{0}")]
    Account(#[from] account::ReadError),
    #[error("{0}")]
    Suspension(#[from] suspension::Error),
    #[error("{0}")]
    Transfer(#[from] identity::transfer::Error),
    #[error("{0}")]
    Avatar(#[from] identity::avatar::Error),
    #[error("account read query failed: {0}")]
    Query(#[from] Query),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("{0}")]
    Work(#[from] work::Error),
    #[error("account read budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("account read transaction end failed: {0}")]
    End(#[source] EndError),
    #[error("account read rejection rollback failed")]
    Rollback {
        #[source]
        cause: Box<Error>,
        rollback: EndError,
    },
    #[error("account read and connection termination failed")]
    Combined {
        #[source]
        cause: Box<Error>,
        termination: platform::database::Error,
    },
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(error.into())
    }
}

impl From<crate::history::Error> for Error {
    fn from(error: crate::history::Error) -> Self {
        Self::Role(role::Error::History(error))
    }
}

impl crate::database::read::Failure for Error {
    fn changed(&self) -> bool {
        crate::database::read::changed(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Invalid,
    Denied,
    Absent,
    Unavailable,
}

impl Error {
    pub fn failure(&self) -> Failure {
        match self {
            Self::Current(current::Error::Invalid)
            | Self::Role(role::Error::Current(current::Error::Invalid)) => Failure::Invalid,
            Self::Role(role::Error::Denied) => Failure::Denied,
            Self::Absent => Failure::Absent,
            _ => Failure::Unavailable,
        }
    }
    fn combine(self, result: Result<(), platform::database::Error>) -> Self {
        match result {
            Ok(()) => self,
            Err(termination) => Self::Combined {
                cause: Box::new(self),
                termination,
            },
        }
    }
    fn unreliable(&self) -> bool {
        match self {
            Self::Current(error) => error.unreliable(),
            Self::Role(error) => error.unreliable(),
            Self::Appointment(error) => error.unreliable(),
            Self::Account(error) => error.unreliable(),
            Self::Suspension(error) => error.unreliable(),
            Self::Transfer(error) => error.unreliable(),
            Self::Avatar(error) => error.unreliable(),
            Self::Combined { cause, .. } | Self::Rollback { cause, .. } => cause.unreliable(),
            _ => false,
        }
    }
}
