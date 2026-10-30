#![allow(clippy::expect_used)]

use std::time::Duration;

use fixture::database_proxy::{Fault, Proxy};
use serde_json::{Value, json};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{
    access::{appointment, role},
    account::{
        Candidate,
        tests::{assign, register_manager},
        transfer::{Failure, Input},
    },
    identity::{
        authentication::{self, Database, Supplier, server},
        message, operation, transfer,
        transfer::tests::{bind_to, start, start_on},
    },
};

fn operation() -> operation::Id {
    Uuid::new_v4().to_string().parse().expect("operation ID")
}

fn confirmation(candidate: &Candidate) -> Value {
    json!({"personId": candidate.identity().person_id(), "revisionId": candidate.identity().revision_id(), "mobile": "13800000000"})
}

fn input(candidate: &Candidate) -> Input {
    let mut value = confirmation(candidate);
    value["verified"] = json!(true);
    serde_json::from_value(value).expect("explicit verified confirmation")
}

fn completion(
    operation_id: operation::Id,
    message_id: message::Id,
    code: &str,
) -> transfer::completion::Input {
    transfer::completion::Input::new(
        operation_id,
        "13600000000".parse().expect("new phone"),
        message_id,
        code.parse().expect("code"),
    )
}

fn sending(operation_id: operation::Id, mobile: &str) -> authentication::Input {
    authentication::Input::new(
        operation_id,
        mobile.parse().expect("phone"),
        operation_id.to_string().parse().expect("token"),
        "192.0.2.12".parse().expect("source"),
    )
}

fn role_input(level: i16, permissions: &[&str]) -> role::Input {
    serde_json::from_value(json!({"name":"Manager", "level":level, "permissions":permissions}))
        .expect("role definition")
}

#[test]
fn h2_tombstone_assisted_transfer_sql_rejects_a_cancelled_target_after_a_valid_control() {
    use tokio_postgres::error::SqlState;

    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(40), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let sms = fixture::supplier::Supplier::start().await;
            let (context, services) = start(&database, &supplier, &sms).await;
            let manager = role::tests::actor(&database, &services, &mut supplier).await;
            let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
            let subject = services.current().read(&target).await.expect("registered target");
            let operator = services.current().read(&manager).await.expect("normal manager");
            let writer = database.connections(&database.user).recover().await.expect("runtime writer");
            let client = writer.client();
            assert_eq!(client.query_one("SELECT current_user::text", &[]).await.expect("actual SQL identity").get::<_, String>(0), database.user);
            let statement = "INSERT INTO identity.transfers(id,person_id,operator_id,revision_id,session_id,message_id,created_at,operation_id,hash,key_id) VALUES($1,$2,$3,$4,$5,NULL,$6,$7,decode(repeat('00',32),'hex'),0)";
            let id = Uuid::new_v4();
            let original = operation();
            client.batch_execute("BEGIN").await.expect("qualified control transaction");
            let now = context.sample().await.expect("trusted control time");
            assert_eq!(client.execute(statement, &[&id, &subject.person_id(), &operator.revision_id(), &subject.revision_id(), &manager.session_id(), &now, &original]).await.expect("complete assisted transfer accepts a current target"), 1);
            client.batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await.expect("control satisfies deferred constraints");
            client.batch_execute("ROLLBACK").await.expect("control leaves no transfer");
            let confirmation = serde_json::from_value(json!({"accountId":subject.id(),"personId":subject.person_id(),"revisionId":subject.revision_id()})).expect("self cancellation confirmation");
            Box::pin(services.cancellation().cancel(target, operation(), confirmation)).await.expect("real target cancellation");
            assert!(client.query_one("SELECT cancellation_id IS NOT NULL FROM identity.accounts WHERE id=$1", &[&subject.id()]).await.expect("retained account tombstone").get::<_, bool>(0));
            services.current().read(&manager).await.expect("manager remains normal");
            client.batch_execute("BEGIN").await.expect("cancelled target transaction");
            let now = context.sample().await.expect("trusted rejection time");
            let error = client.execute(statement, &[&id, &subject.person_id(), &operator.revision_id(), &subject.revision_id(), &manager.session_id(), &now, &original]).await.expect_err("runtime SQL refuses the tombstone as a current target");
            assert_eq!(error.code(), Some(&SqlState::CHECK_VIOLATION));
            assert_eq!(error.as_db_error().expect("target qualification error").message(), "transfer requires current operator and target accounts");
            client.batch_execute("ROLLBACK").await.expect("rejected transaction ended");
            assert!(!client.query_one("SELECT EXISTS(SELECT 1 FROM identity.transfers WHERE operation_id=$1)", &[&original]).await.expect("neither attempt writes a transfer").get::<_, bool>(0));
            writer.terminate().await.expect("runtime writer ended");
            services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
            assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
            supplier.stop().await; sms.stop().await;
        })).await.expect("bounded cancelled target SQL regression");
    }));
}

#[test]
fn assisted_transfer_completes_normal_and_paused_targets_without_revoking_the_manager() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for paused in [false, true] {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(server.clone()).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = start(&database, &supplier, &sms).await;
                let manager = Box::pin(role::tests::actor(&database, &services, &mut supplier)).await;
                let other = Box::pin(role::tests::actor(&database, &services, &mut supplier)).await;
                let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let candidate = services.account().candidate(&manager, "13800000000".parse().expect("phone")).await.expect("target");
                let account_id = candidate.identity().id();
                let observer = database.connections(&database.user).recover().await.expect("independent SQL reader");
                let establishment = operation();
                let work = Box::pin(async {
                    if paused {
                        services.suspension().suspend(manager.clone(), operation(), account_id, serde_json::from_value(confirmation(&candidate)).expect("pause input")).await.expect("real target suspension");
                    }
                    let transfer = services.account().create_transfer(manager.clone(), establishment, account_id, input(&candidate)).await.expect("assisted establishment");
                    assert_eq!(services.account().create_transfer(manager.clone(), establishment, account_id, input(&candidate)).await.expect("original establishment").id(), transfer.id());
                    let row = observer.client().query_one("SELECT transfer.message_id IS NULL, transfer.person_id=$2, transfer.revision_id=$3, transfer.session_id=$4, operator.person_id <> transfer.person_id, transfer.created_at+interval '10 minutes'=$5 FROM identity.transfers transfer JOIN identity.revisions operator ON operator.id=transfer.operator_id WHERE transfer.id=$1", &[&transfer.id(), &candidate.identity().person_id(), &candidate.identity().revision_id(), &manager.session_id(), &transfer.expires_at()]).await.expect("independent original binding");
                    for column in 0..6 { assert!(row.get::<_,bool>(column)); }
                    let mut changed = confirmation(&candidate); changed["verified"]=json!(true); changed["mobile"]=json!("13700000000");
                    assert_eq!(services.account().create_transfer(manager.clone(), establishment, account_id, serde_json::from_value(changed).expect("different valid confirmation")).await.expect_err("original phone remains bound").failure(), Failure::Mismatch);
                    transfer
                });
                let transfer = tokio::select! { biased;
                    _ = supplier.observe() => panic!("establishment never verifies"),
                    _ = sms.request() => panic!("establishment never sends SMS"),
                    result = work => result,
                };
                let before = observer.client().query_one("SELECT jsonb_build_object('id',id,'person_id',person_id,'id_number',id_number,'avatar',avatar,'wechat',wechat,'created_at',created_at,'suspension_id',suspension_id,'cancellation_id',cancellation_id)::text AS business,sequence,version FROM identity.accounts WHERE id=$1", &[&account_id]).await.expect("account identity and current suspension before replacement");
                let (_, sent, code) = bind_to(&database, &services, &manager, transfer.id(), &mut supplier, &mut sms, "13600000000").await;
                let operation_id = operation();
                let work = Box::pin(async {
                    let wrong = if code == "000000" { "000001" } else { "000000" };
                    let rejected = operation();
                    for _ in 0..2 {
                        assert_eq!(services.transfer().complete_assisted(manager.clone(), account_id, completion(rejected, sent.id(), wrong)).await.expect_err("wrong code").code(), Some("verification_invalid"));
                    }
                    assert_eq!(observer.client().query_one("SELECT failures FROM identity.confirmations WHERE id=$1", &[&sent.id()]).await.expect("one rejected original").get::<_,i16>(0), 1);
                    assert_eq!(services.transfer().complete_assisted(manager.clone(), account_id, completion(rejected, sent.id(), &code)).await.expect_err("same operation cannot change code").code(), Some("request_mismatch"));
                    assert_eq!(services.transfer().complete_assisted(other.clone(), account_id, completion(operation(), sent.id(), &code)).await.expect_err("only establishing session").code(), Some("verification_required"));
                    assert_eq!(services.transfer().complete(manager.clone(), completion(operation(), sent.id(), &code)).await.expect_err("manager cannot apply the target code to self").code(), Some("verification_required"));
                    let completed = services.transfer().complete_assisted(manager.clone(), account_id, completion(operation_id, sent.id(), &code)).await.expect("assisted completion");
                    let hash = authentication::keys().mobile.hash(&"13600000000".parse().expect("new phone")).expect("digest");
                    let row = observer.client().query_one("SELECT completion.id=$2 AND completion.account_id=$3 AND completion.session_id=$4 AND completion.message_id=$5,
                        (account.mobile).hash=$6, jsonb_build_object('id',account.id,'person_id',account.person_id,'id_number',account.id_number,'avatar',account.avatar,'wechat',account.wechat,'created_at',account.created_at,'suspension_id',account.suspension_id,'cancellation_id',account.cancellation_id)::text,
                        (SELECT count(*) FROM identity.revocations WHERE mode='transfer' AND cause_id=completion.id),
                        NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$4),
                        NOT EXISTS(SELECT 1 FROM identity.sessions session WHERE session.account_id=$3 AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=session.id)),
                        NOT EXISTS(SELECT 1 FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=$3),
                        account.suspension_id IS NOT NULL,
                        account.sequence=$7::bigint+1 AND account.version=$8::bigint+1 AND account.changed_at=completion.completed_at
                        FROM identity.replacements completion JOIN identity.accounts account ON account.id=completion.account_id WHERE completion.operation_id=$1", &[&operation_id, &transfer.id(), &account_id, &manager.session_id(), &sent.id(), &hash, &before.get::<_,i64>("sequence"), &before.get::<_,i64>("version")]).await.expect("persistent effects before replay");
                    assert!(row.get::<_,bool>(0)); assert!(row.get::<_,bool>(1)); assert_eq!(row.get::<_,String>(2), before.get::<_,String>("business")); assert!(row.get::<_,bool>(8));
                    assert_eq!(row.get::<_,i64>(3), if paused { 0 } else { 1 });
                    for column in 4..7 { assert!(row.get::<_,bool>(column)); }
                    assert_eq!(row.get::<_,bool>(7), paused);
                    services.current().read(&manager).await.expect("manager stays authenticated");
                    services.current().read(&other).await.expect("other manager session stays authenticated");
                    assert!(services.current().read(&target).await.is_err());
                    assert_eq!(services.transfer().complete_assisted(manager.clone(), account_id, completion(operation_id, sent.id(), &code)).await.expect("original completion"), completed);
                    assert_eq!(services.transfer().complete_assisted(manager.clone(), account_id, completion(operation_id, sent.id(), wrong)).await.expect_err("changed original code").code(), Some("request_mismatch"));
                    assert!(matches!(services.account().fetch_transfer(&manager, transfer.id()).await.expect("retained result"), transfer::State::Completed { .. }));
                    services.drain().await.expect("work drained"); context.stop().await.expect("context ended");
                    completed
                });
                let completed = tokio::select! { biased;
                    _ = supplier.observe() => panic!("completion and replay never verify"),
                    _ = sms.request() => panic!("completion and replay never send SMS"),
                    result = work => result,
                };
                let (context, services) = start(&database, &supplier, &sms).await;
                let work = Box::pin(async {
                    assert_eq!(services.transfer().complete_assisted(manager.clone(), account_id, completion(operation_id, sent.id(), &code)).await.expect("restart retains original proof and effects"), completed);
                    services.drain().await.expect("restart drained"); context.stop().await.expect("restart context ended");
                });
                tokio::select! { biased; _ = supplier.observe() => panic!("restart never verifies"), _ = sms.request() => panic!("restart never sends SMS"), () = work => {} }
                observer.terminate().await.expect("observer ended"); sms.stop().await; supplier.stop().await;
            })).await.expect("bounded assisted lifecycle");
        }
    }));
}

#[test]
fn assisted_transfer_rechecks_exact_permission_and_strict_rank_at_each_new_or_replayed_action() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(40), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let (context, services) = start(&database, &supplier, &sms).await;
            let administrator = Box::pin(role::tests::actor(&database, &services, &mut supplier)).await;
            let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
            let manager = Box::pin(register_manager(&database, &services, &mut supplier)).await;
            let target_role = services.role().create(administrator.clone(), operation(), serde_json::from_value(json!({"name":"Target", "level":5, "permissions":[]})).expect("target role")).await.expect("role");
            let mut managed = services.role().create(administrator.clone(), operation(), role_input(10, &["institution.accounts"])).await.expect("manager role");
            Box::pin(assign(&services, &administrator, "13800000000", &target_role)).await;
            Box::pin(assign(&services, &administrator, "13700000000", &managed)).await;
            let candidate = services.account().candidate(&manager, "13800000000".parse().expect("phone")).await.expect("candidate");
            let account_id = candidate.identity().id();
            let establishment = operation();
            let transfer = services.account().create_transfer(manager.clone(), establishment, account_id, input(&candidate)).await.expect("established");
            let (sending_id, sent, code) = bind_to(&database, &services, &manager, transfer.id(), &mut supplier, &mut sms, "13600000000").await;
            let observer = database.connections(&database.user).recover().await.expect("independent facts");
            let work = Box::pin(async {
                for (level, permissions, expected, send_failure) in [
                    (5, vec!["institution.accounts"], Failure::Absent, authentication::Failure::Absent),
                    (4, vec!["institution.accounts"], Failure::Absent, authentication::Failure::Absent),
                    (10, vec!["institution.roles"], Failure::Denied, authentication::Failure::Denied),
                ] {
                    managed = Box::pin(services.role().update(administrator.clone(), operation(), managed.id(), role::Update::new(managed.change_id(), role_input(level, &permissions)))).await.expect("actual qualification change");
                    for id in [establishment, operation()] {
                        assert_eq!(Box::pin(services.account().create_transfer(manager.clone(), id, account_id, input(&candidate))).await.expect_err("current manager qualification required").failure(), expected);
                    }
                    for id in [sending_id, operation()] {
                        assert_eq!(Box::pin(services.authentication().send_binding(manager.clone(), transfer.id(), sending(id, "13600000000"))).await.expect_err("current permission required before replay or sending").failure(), send_failure);
                    }
                    let failure = Box::pin(services.transfer().complete_assisted(manager.clone(), account_id, completion(operation(), sent.id(), &code))).await.expect_err("completion rechecks current manager").code();
                    assert_eq!(failure, Some(if expected == Failure::Denied { "permission_denied" } else { "target_unavailable" }));
                    assert!(observer.client().query_one("SELECT (SELECT count(*) FROM identity.transfers)=1 AND NOT EXISTS(SELECT 1 FROM identity.replacements) AND NOT EXISTS(SELECT 1 FROM identity.rejections) AND (SELECT failures FROM identity.confirmations WHERE id=$1)=0", &[&sent.id()]).await.expect("no new facts after refusals").get::<_,bool>(0));
                }
                managed = Box::pin(services.role().update(administrator.clone(), operation(), managed.id(), role::Update::new(managed.change_id(), role_input(10, &["institution.accounts"])))).await.expect("restore management");
                assert_eq!(i16::from(managed.level()), 10);
                assert_eq!(Box::pin(services.account().create_transfer(manager.clone(), operation(), manager.account_id(), input(&candidate))).await.expect_err("cannot assist self").failure(), Failure::Absent);
                let completed = operation();
                Box::pin(services.transfer().complete_assisted(manager.clone(), account_id, completion(completed, sent.id(), &code))).await.expect("restored authority completes the original transfer");
                Box::pin(services.role().update(administrator.clone(), operation(), managed.id(), role::Update::new(managed.change_id(), role_input(10, &[])))).await.expect("later permission loss");
                assert_eq!(Box::pin(services.transfer().complete_assisted(manager.clone(), account_id, completion(completed, sent.id(), &code))).await.expect_err("original success is not a permanent reading permission").code(), Some("permission_denied"));
                assert!(services.current().read(&target).await.is_err());
                services.drain().await.expect("drained"); context.stop().await.expect("context ended");
            });
            tokio::select! { biased; _ = supplier.observe() => panic!("refusals and original reads never verify"), _ = sms.request() => panic!("refusals and completion never send SMS"), () = work => {} }
            observer.terminate().await.expect("observer ended"); sms.stop().await; supplier.stop().await;
        })).await.expect("bounded dynamic management qualification");
    }));
}

#[test]
fn assisted_binding_stops_after_permission_loss_but_preserves_a_started_send() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for sending in [false, true] {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(server.clone()).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = start(&database, &supplier, &sms).await;
                let administrator = Box::pin(role::tests::actor(&database, &services, &mut supplier)).await;
                Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let manager = Box::pin(register_manager(&database, &services, &mut supplier)).await;
                let mut managed = services.role().create(administrator.clone(), operation(), role_input(10, &["institution.accounts"])).await.expect("manager role");
                Box::pin(assign(&services, &administrator, "13700000000", &managed)).await;
                let candidate = services.account().candidate(&manager, "13800000000".parse().expect("phone")).await.expect("target");
                let transfer = services.account().create_transfer(manager.clone(), operation(), candidate.identity().id(), input(&candidate)).await.expect("assisted transfer");
                let operation_id = operation();
                let service = services.authentication().clone(); let original = manager.clone(); let id = transfer.id();
                let mut task = tokio::spawn(async move { service.send_binding(original, id, crate::account::transfer::tests::sending(operation_id, "13600000000")).await });
                let challenge = tokio::select! { request = supplier.request() => request, result = &mut task => panic!("sender ended before verification: {result:?}") };
                let response = json!({"success":true,"challenge_ts":database.now().await.to_rfc3339(),"hostname":"example.com","action":"send_mobile_binding_code","cdata":operation_id.to_string()}).to_string();
                if sending {
                    challenge.reply(200, response);
                    let request = tokio::select! { request = sms.request() => request, result = &mut task => panic!("sender ended before SMS: {result:?}") };
                    managed = services.role().update(administrator.clone(), operation(), managed.id(), role::Update::new(managed.change_id(), role_input(10, &[]))).await.expect("withdraw permission during SMS");
                    transfer::tests::deliver(request, "13600000000", "bind");
                } else {
                    managed = services.role().update(administrator.clone(), operation(), managed.id(), role::Update::new(managed.change_id(), role_input(10, &[]))).await.expect("withdraw permission during human verification");
                    challenge.reply(200, response);
                }
                let observer = database.connections(&database.user).recover().await.expect("independent reader");
                let work = Box::pin(async {
                    let result = task.await.expect("accepted work");
                    if sending { result.expect("already started send saves its conclusion"); }
                    else { assert_eq!(result.expect_err("no permission for SMS preparation").failure(), authentication::Failure::Denied); }
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM identity.messages message JOIN identity.challenges challenge ON challenge.id=message.challenge_id WHERE challenge.operation_id=$1), (SELECT count(*) FROM identity.claims claim JOIN identity.messages message ON message.id=claim.id JOIN identity.challenges challenge ON challenge.id=message.challenge_id WHERE challenge.operation_id=$1)", &[&operation_id]).await.expect("stored permit and claim before any replay");
                    assert_eq!(row.get::<_,i64>(0), i64::from(sending)); assert_eq!(row.get::<_,i64>(1), i64::from(sending));
                    services.role().update(administrator.clone(), operation(), managed.id(), role::Update::new(managed.change_id(), role_input(10, &["institution.accounts"]))).await.expect("restore permission");
                    if sending {
                        let restored = services.authentication().send_binding(manager.clone(), transfer.id(), crate::account::transfer::tests::sending(operation_id, "13600000000")).await.expect("read original message");
                        assert!(matches!(restored.status(), message::Status::Sent { .. }));
                    }
                    services.drain().await.expect("original work drained"); context.stop().await.expect("context ended");
                });
                tokio::select! { biased; _ = supplier.observe() => panic!("no repeated verification"), _ = sms.request() => panic!("no additional SMS"), () = work => {} }
                observer.terminate().await.expect("reader ended"); sms.stop().await; supplier.stop().await;
            })).await.expect("bounded in-flight management change");
        }
    }));
}

#[test]
fn assisted_transfer_recovers_establishment_and_completion_without_extra_external_calls() {
    enum Replay {
        Establishment,
        Completion {
            message_id: message::Id,
            code: String,
        },
    }
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for completion_phase in [false, true] {
            for (fault, delayed, rolled_back) in [(Fault::LostCommit, false, false), (Fault::DelayedCommit, true, false), (Fault::RolledBack, false, true)] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(server.clone()).await;
                    let mut supplier = Supplier::new().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier, &sms).await;
                    let manager = Box::pin(role::tests::actor(&database, &services, &mut supplier)).await;
                    let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let candidate = services.account().candidate(&manager, "13800000000".parse().expect("phone")).await.expect("candidate");
                    let account_id = candidate.identity().id();
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let operation_id = operation();
                    let (task, pending) = if completion_phase {
                        let transfer = services.account().create_transfer(manager.clone(), operation(), account_id, input(&candidate)).await.expect("established");
                        let (_, sent, code) = bind_to(&database, &services, &manager, transfer.id(), &mut supplier, &mut sms, "13600000000").await;
                        let service = services.transfer().clone(); let actor = manager.clone(); let command = completion(operation_id, sent.id(), &code);
                        proxy.arm(fault);
                        (tokio::spawn(async move {
                            let result = service.complete_assisted(actor, account_id, command).await;
                            if rolled_back { assert!(matches!(result, Err(transfer::completion::Error::Unavailable(_))), "{result:?}"); }
                            else { result.expect("lost completion response recovered"); }
                        }), Replay::Completion { message_id: sent.id(), code })
                    } else {
                        let service = services.account().clone(); let actor = manager.clone(); let command = input(&candidate);
                        proxy.arm(fault);
                        (tokio::spawn(async move {
                            let result = service.create_transfer(actor, operation_id, account_id, command).await;
                            if rolled_back { assert!(matches!(result, Err(crate::account::transfer::Error::NotCommitted { .. })), "{result:?}"); }
                            else { result.expect("lost establishment response recovered"); }
                        }), Replay::Establishment)
                    };
                    let work = Box::pin(async {
                        timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("original commit actually intercepted");
                        if delayed {
                            timeout(Duration::from_secs(5), async { loop {
                                if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()))", &[]).await.expect("independent operation lock wait").get::<_,bool>(0) { break; }
                                tokio::task::yield_now().await;
                            }}).await.expect("recovery waits for original transaction");
                            task.abort(); assert!(task.await.expect_err("caller cancellation").is_cancelled());
                            proxy.release();
                        } else {
                            task.await.expect("work task");
                        }
                        if !rolled_back {
                            timeout(Duration::from_secs(5), async { loop {
                                if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.transfers WHERE operation_id=$1) OR EXISTS(SELECT 1 FROM identity.replacements WHERE operation_id=$1)", &[&operation_id]).await.expect("observe committed original before replay").get::<_,bool>(0) { break; }
                                tokio::task::yield_now().await;
                            }}).await.expect("committed outcome");
                        }
                        let row = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.transfers WHERE operation_id=$1), EXISTS(SELECT 1 FROM identity.replacements WHERE operation_id=$1), EXISTS(SELECT 1 FROM identity.revocations WHERE id=$2), EXISTS(SELECT 1 FROM identity.revocations WHERE id=$3)", &[&operation_id, &manager.session_id(), &target.session_id()]).await.expect("persistent effects before explicit retry");
                        assert_eq!(row.get::<_,bool>(0), !completion_phase && !rolled_back);
                        assert_eq!(row.get::<_,bool>(1), completion_phase && !rolled_back);
                        assert!(!row.get::<_,bool>(2)); assert_eq!(row.get::<_,bool>(3), completion_phase && !rolled_back);
                        match pending {
                            Replay::Completion { message_id, code } => { services.transfer().complete_assisted(manager.clone(), account_id, completion(operation_id, message_id, &code)).await.expect("read original or explicitly retry confirmed rollback"); }
                            Replay::Establishment => { services.account().create_transfer(manager.clone(), operation_id, account_id, input(&candidate)).await.expect("read original or explicitly retry confirmed rollback"); }
                        }
                        services.current().read(&manager).await.expect("manager survives recovery");
                        services.drain().await.expect("accepted work drains after recovery"); context.stop().await.expect("runtime ended");
                    });
                    tokio::select! { biased; _ = supplier.observe() => panic!("recovery never repeats human verification"), _ = sms.request() => panic!("recovery never repeats SMS"), () = work => {} }
                    observer.terminate().await.expect("observer ended"); sms.stop().await; supplier.stop().await; proxy.stop().await;
                })).await.expect("bounded assisted recovery");
            }
        }
    }));
}

#[test]
fn assisted_transfer_keeps_the_original_operator_but_rejects_a_changed_target_revision() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for target_changed in [false, true] {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(server.clone()).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = start(&database, &supplier, &sms).await;
                let administrator = Box::pin(role::tests::actor(&database, &services, &mut supplier)).await;
                Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let manager = Box::pin(register_manager(&database, &services, &mut supplier)).await;
                let managed = services.role().create(administrator.clone(), operation(), role_input(10, &["institution.accounts"])).await.expect("manager role");
                Box::pin(assign(&services, &administrator, "13700000000", &managed)).await;
                let candidate = services.account().candidate(&manager, "13800000000".parse().expect("phone")).await.expect("target");
                let account_id = candidate.identity().id(); let establishment = operation();
                let transfer = services.account().create_transfer(manager.clone(), establishment, account_id, input(&candidate)).await.expect("established");
                let (mobile, id_number) = if target_changed { ("13800000000", "00000020000229007X") } else { ("13700000000", "000000200002290029") };
                let selected = services.correction().candidate_by_mobile(&administrator, mobile.parse().expect("phone")).await.expect("real correction target");
                Box::pin(services.correction().correct(administrator.clone(), operation(), serde_json::from_value(json!({"target": selected.target(), "name":"Revised Person", "idNumber":id_number})).expect("correction input"))).await.expect("actual revision change");
                let observer = database.connections(&database.user).recover().await.expect("independent facts");
                if target_changed {
                    let work = Box::pin(async {
                        assert_eq!(services.authentication().send_binding(manager.clone(), transfer.id(), sending(operation(), "13600000000")).await.expect_err("old target confirmation cannot authorize new SMS").failure(), authentication::Failure::Verification);
                        assert_eq!(services.account().create_transfer(manager.clone(), operation(), account_id, input(&candidate)).await.expect_err("new establishment rechecks revision").failure(), Failure::TargetChanged);
                        assert_eq!(services.account().create_transfer(manager.clone(), establishment, account_id, input(&candidate)).await.expect("original accepted confirmation survives later correction").id(), transfer.id());
                        assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.challenges WHERE transfer_id=$1)", &[&transfer.id()]).await.expect("no new challenge").get::<_,bool>(0));
                        services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                    });
                    tokio::select! { biased; _=supplier.observe()=>panic!("changed target cannot verify"), _=sms.request()=>panic!("changed target cannot send"), ()=work=>{} }
                } else {
                    let (_, sent, code) = bind_to(&database, &services, &manager, transfer.id(), &mut supplier, &mut sms, "13600000000").await;
                    let work = Box::pin(async {
                        assert!(observer.client().query_one("SELECT confirmation.operator_id <> transfer.operator_id AND operator.person_id = original.person_id FROM identity.confirmations confirmation JOIN identity.transfers transfer ON transfer.id=$1 JOIN identity.revisions operator ON operator.id=confirmation.operator_id JOIN identity.revisions original ON original.id=transfer.operator_id WHERE confirmation.id=$2", &[&transfer.id(), &sent.id()]).await.expect("code binds the current operator version while transfer keeps the original").get::<_,bool>(0));
                        services.transfer().complete_assisted(manager.clone(), account_id, completion(operation(), sent.id(), &code)).await.expect("same person and establishing session continue after operator correction");
                        assert!(observer.client().query_one("SELECT completion.operator_id=transfer.operator_id AND completion.session_id=transfer.session_id FROM identity.replacements completion JOIN identity.transfers transfer ON transfer.id=completion.id WHERE completion.id=$1", &[&transfer.id()]).await.expect("completion retains original operator and session").get::<_,bool>(0));
                        services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                    });
                    tokio::select! { biased; _=supplier.observe()=>panic!("completion cannot verify"), _=sms.request()=>panic!("completion cannot send"), ()=work=>{} }
                }
                observer.terminate().await.expect("observer ended"); sms.stop().await; supplier.stop().await;
            })).await.expect("bounded revision binding");
        }
    }));
}

#[test]
fn assisted_establishment_matches_an_independent_input_proof_vector() {
    let proof = transfer::Assistance {
        operation_id: Uuid::from_u128(1).to_string().parse().expect("operation"),
        operator_id: serde_json::from_value(json!(Uuid::from_u128(2))).expect("operator"),
        person_id: serde_json::from_value(json!(Uuid::from_u128(3))).expect("person"),
        revision_id: serde_json::from_value(json!(Uuid::from_u128(4))).expect("revision"),
        session_id: Uuid::from_u128(5).to_string().parse().expect("session"),
        mobile: &"13800000000".parse().expect("phone"),
        verified: &serde_json::from_value(json!(true)).expect("explicit verification"),
    };
    // Independently calculated with Python struct, uuid, hashlib and hmac:
    // Python HKDF-SHA-256 of the fixture root (32 bytes of 1; salt "nucleus/key/v1", info "proof"), then hmac.
    assert_eq!(
        hex::encode(
            authentication::keys()
                .input
                .active()
                .sign(&platform::cryptography::Encoded::new(&proof).expect("encoding"))
                .as_ref()
        ),
        "8d01fc31e70d3914a30cd734f6fe241dcefa001c7b20fd022fc638935758b13a"
    );
}

#[test]
fn assisted_binding_rechecks_a_transfer_replaced_while_waiting_for_the_account_lock() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(30), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let (context, services) = start(&database, &supplier, &sms).await;
            let manager = Box::pin(role::tests::actor(&database, &services, &mut supplier)).await;
            Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
            let candidate = services.account().candidate(&manager, "13800000000".parse().expect("phone")).await.expect("target");
            let transfer = services.account().create_transfer(manager.clone(), operation(), candidate.identity().id(), input(&candidate)).await.expect("established");
            let owner = database.connections(&database.user).recover().await.expect("runtime writer");
            let observer = database.connections(&database.user).recover().await.expect("independent SQL observer");
            let client = owner.client();
            let work = Box::pin(async {
                client.batch_execute("BEGIN").await.expect("hold competing establishment");
                crate::database::transaction::shared(client, b"access/scope", b"institution").await.expect("management scope");
                manager.lock_accounts(client, candidate.identity().id()).await.expect("sorted account locks");
                let blocker: i32 = client.query_one("SELECT pg_backend_pid()", &[]).await.expect("actual account owner").get(0);
                let authentication = services.authentication().clone();
                let actor = manager.clone();
                let operation_id = operation();
                let pending = tokio::spawn(async move { authentication.send_binding(actor, transfer.id(), sending(operation_id, "13600000000")).await });
                timeout(Duration::from_secs(3), async {
                    loop {
                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity waiter JOIN pg_stat_activity original ON original.pid=ANY(pg_blocking_pids(waiter.pid)) WHERE waiter.datname=current_database() AND original.datname=current_database() AND original.pid=$1 AND waiter.wait_event_type='Lock' AND waiter.query='SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE')", &[&blocker]).await.expect("C1 waits on the actual account locks").get::<_,bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("original transfer was located before the account wait");
                let keys = authentication::keys();
                let target = transfer::Target::assisted(client, &services.account().roles, services.current(), &keys, &manager, candidate.identity().id()).await.expect("current qualification under held locks");
                let replacement = transfer::Original::establish(client, &keys, &input(&candidate).proof(&manager, operation(), target.operator_id()), target.operator_revision_id(), target.observed_at()).await.expect("later assisted transfer passes runtime guards");
                client.batch_execute("COMMIT").await.expect("replace the transfer before releasing the locks");
                assert!(observer.client().query_one("SELECT later.created_at>original.created_at FROM identity.transfers original CROSS JOIN identity.transfers later WHERE original.id=$1 AND later.id=$2", &[&transfer.id(), &replacement.id()]).await.expect("committed supersession").get::<_,bool>(0));
                assert_eq!(pending.await.expect("C1 task").expect_err("superseded input is a business refusal").failure(), authentication::Failure::Verification);
                assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.challenges WHERE operation_id=$1)", &[&operation_id]).await.expect("no challenge accepted after supersession").get::<_,bool>(0));
                services.drain().await.expect("all supervised work ended");
                context.stop().await.expect("runtime ended");
            });
            tokio::select! { biased; _=supplier.observe()=>panic!("superseded transfer cannot verify"), _=sms.request()=>panic!("superseded transfer cannot send"), ()=work=>{} }
            observer.terminate().await.expect("observer ended"); owner.terminate().await.expect("writer ended"); sms.stop().await; supplier.stop().await;
        })).await.expect("bounded lock-wait supersession");
    }));
}

#[test]
fn assisted_binding_waits_for_management_without_blocking_self_service() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(40), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let (context, services) = start(&database, &supplier, &sms).await;
            let manager = Box::pin(role::tests::actor(&database, &services, &mut supplier)).await;
            Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
            authentication::session::tests::age(&database, services.authentication(), chrono::TimeDelta::seconds(61)).await;
            let (_, original, code) = transfer::tests::send_to(&database, &services, &manager, &mut supplier, &mut sms, "13900000000").await;
            let own = Box::pin(services.transfer().create(manager.clone(), transfer::Input::new(operation(), original.id(), code.parse().expect("original phone code")))).await.expect("self-service transfer");
            let candidate = services.account().candidate(&manager, "13800000000".parse().expect("phone")).await.expect("managed target");
            let assisted = Box::pin(services.account().create_transfer(manager.clone(), operation(), candidate.identity().id(), input(&candidate))).await.expect("assisted transfer");
            let owner = database.connections(&database.user).recover().await.expect("runtime lock owner");
            let observer = database.connections(&database.user).recover().await.expect("independent SQL observer");
            // Independently derived from the documented LP32 SHA-256 lock encoding.
            owner.client().batch_execute("BEGIN; SELECT pg_catalog.pg_advisory_xact_lock(-3487418652284273221)").await.expect("exclusive institution scope");
            let blocker: i32 = owner.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("actual scope owner").get(0);
            let authentication = services.authentication().clone();
            let actor = manager.clone();
            let operation_id = operation();
            let pending = tokio::spawn(async move { authentication.send_binding(actor, assisted.id(), sending(operation_id, "13500000000")).await });
            let waiting = timeout(Duration::from_secs(3), async {
                loop {
                    if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND wait_event_type='Lock' AND query='SELECT pg_catalog.pg_advisory_xact_lock_shared($1)')", &[&blocker]).await.expect("assisted C1 waits on the actual scope lock").get::<_,bool>(0) { break; }
                    tokio::task::yield_now().await;
                }
            });
            tokio::select! { biased;
                _ = supplier.observe() => panic!("assisted send cannot verify before taking the scope lock"),
                _ = sms.request() => panic!("assisted send cannot send before taking the scope lock"),
                result = waiting => result.expect("bounded scope wait"),
            }
            let (sending_id, sent, _) = timeout(Duration::from_secs(5), Box::pin(bind_to(&database, &services, &manager, own.id(), &mut supplier, &mut sms, "13600000000"))).await.expect("self-service sends while institution scope is held");
            assert!(matches!(sent.status(), message::Status::Sent { .. }));
            let replay = Box::pin(async {
                assert_eq!(services.authentication().send_binding(manager.clone(), own.id(), sending(sending_id, "13600000000")).await.expect("self-service replay also skips management scope").id(), sent.id());
                assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.challenges challenge JOIN identity.messages message ON message.challenge_id=challenge.id WHERE challenge.operation_id=$1 AND challenge.transfer_id=$2 AND message.id=$3 AND message.status='sent') AND NOT EXISTS(SELECT 1 FROM identity.challenges WHERE operation_id=$4)", &[&sending_id, &own.id(), &sent.id(), &operation_id]).await.expect("self-service persisted while assisted admission remains blocked").get::<_,bool>(0));
            });
            tokio::select! { biased;
                _ = supplier.observe() => panic!("blocked assistance and self-service replay cannot verify"),
                _ = sms.request() => panic!("blocked assistance and self-service replay cannot send"),
                result = timeout(Duration::from_secs(3), replay) => result.expect("bounded self-service replay"),
            }
            owner.client().batch_execute("COMMIT").await.expect("release management scope");
            let request = timeout(Duration::from_secs(5), supplier.request()).await.expect("assisted verification after scope release");
            request.reply(200, json!({"success":true,"challenge_ts":database.now().await.to_rfc3339(),"hostname":"example.com","action":"send_mobile_binding_code","cdata":operation_id.to_string()}).to_string());
            let request = timeout(Duration::from_secs(5), sms.request()).await.expect("assisted SMS after scope release");
            transfer::tests::deliver(request, "13500000000", "bind");
            let finish = Box::pin(async {
                let sent = pending.await.expect("assisted task").expect("assisted send");
                assert!(matches!(sent.status(), message::Status::Sent { .. }));
                assert_eq!(services.authentication().send_binding(manager.clone(), assisted.id(), sending(operation_id, "13500000000")).await.expect("assisted replay").id(), sent.id());
                assert!(observer.client().query_one("SELECT challenge.transfer_id=$2 AND challenge.person_id=$3 AND challenge.session_id=$4 AND message.status='sent' FROM identity.challenges challenge JOIN identity.messages message ON message.challenge_id=challenge.id WHERE challenge.operation_id=$1", &[&operation_id, &assisted.id(), &candidate.identity().person_id(), &manager.session_id()]).await.expect("assisted binding survives the wait").get::<_,bool>(0));
                services.drain().await.expect("all work drained"); context.stop().await.expect("runtime ended");
            });
            tokio::select! { biased;
                _ = supplier.observe() => panic!("no further verification during replay and drain"),
                _ = sms.request() => panic!("no further SMS during replay and drain"),
                () = finish => {},
            }
            observer.terminate().await.expect("observer ended"); owner.terminate().await.expect("scope owner ended"); sms.stop().await; supplier.stop().await;
        })).await.expect("bounded management and self-service concurrency");
    }));
}

#[test]
fn assisted_completion_rejects_a_target_corrected_after_the_new_phone_code() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(30), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let (context, services) = start(&database, &supplier, &sms).await;
            let manager = Box::pin(role::tests::actor(&database, &services, &mut supplier)).await;
            let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
            let candidate = services.account().candidate(&manager, "13800000000".parse().expect("phone")).await.expect("target");
            let account_id = candidate.identity().id();
            let transfer = services.account().create_transfer(manager.clone(), operation(), account_id, input(&candidate)).await.expect("established");
            let (_, sent, code) = bind_to(&database, &services, &manager, transfer.id(), &mut supplier, &mut sms, "13600000000").await;
            let observer = database.connections(&database.user).recover().await.expect("independent SQL observer");
            let before: String = observer.client().query_one("SELECT mobile::text FROM identity.accounts WHERE id=$1", &[&account_id]).await.expect("original phone envelope").get(0);
            let work = Box::pin(async {
                let selected = services.correction().candidate_by_mobile(&manager, "13800000000".parse().expect("phone")).await.expect("correction target");
                Box::pin(services.correction().correct(manager.clone(), operation(), serde_json::from_value(json!({"target":selected.target(), "name":"Revised Person", "idNumber":"00000020000229007X"})).expect("correction input"))).await.expect("actual target revision change");
                assert_eq!(services.transfer().complete_assisted(manager.clone(), account_id, completion(operation(), sent.id(), &code)).await.expect_err("the original confirmed target is no longer current").code(), Some("verification_required"));
                let row = observer.client().query_one("SELECT account.mobile::text, confirmation.failures, NOT EXISTS(SELECT 1 FROM identity.replacements), NOT EXISTS(SELECT 1 FROM identity.revocations), EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$3) FROM identity.accounts account CROSS JOIN identity.confirmations confirmation WHERE account.id=$1 AND confirmation.id=$2", &[&account_id, &sent.id(), &target.session_id()]).await.expect("no partial effects or extra guess");
                assert_eq!(row.get::<_,String>(0), before); assert_eq!(row.get::<_,i16>(1), 0);
                for column in 2..5 { assert!(row.get::<_,bool>(column)); }
                services.drain().await.expect("all supervised work ended"); context.stop().await.expect("runtime ended");
            });
            tokio::select! { biased; _=supplier.observe()=>panic!("completion cannot verify"), _=sms.request()=>panic!("completion cannot send"), ()=work=>{} }
            observer.terminate().await.expect("observer ended"); sms.stop().await; supplier.stop().await;
        })).await.expect("bounded correction after verification");
    }));
}

#[test]
fn assisted_completion_checks_current_management_and_rolls_back_incomplete_effects() {
    use tokio_postgres::error::SqlState;

    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(35), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let (context, services) = start(&database, &supplier, &sms).await;
            let administrator = Box::pin(role::tests::actor(&database, &services, &mut supplier)).await;
            let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
            let manager = Box::pin(register_manager(&database, &services, &mut supplier)).await;
            let mut managed = services.role().create(administrator.clone(), operation(), role_input(10, &["institution.accounts"])).await.expect("manager role");
            Box::pin(assign(&services, &administrator, "13700000000", &managed)).await;
            let candidate = services.account().candidate(&manager, "13800000000".parse().expect("phone")).await.expect("target");
            let account_id = candidate.identity().id();
            let operator = services.current().read(&manager).await.expect("operator");
            let transfer = services.account().create_transfer(manager.clone(), operation(), account_id, input(&candidate)).await.expect("established");
            let (_, sent, code) = bind_to(&database, &services, &manager, transfer.id(), &mut supplier, &mut sms, "13600000000").await;
            let writer = database.connections(&database.user).recover().await.expect("actual runtime identity"); let client = writer.client();
            let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
            let before: String = client.query_one("SELECT jsonb_build_object('account',to_jsonb(account),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session WHERE account_id=account.id),'handoffs',(SELECT jsonb_agg(to_jsonb(handoff) ORDER BY handoff.id) FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=account.id))::text FROM identity.accounts account WHERE id=$1", &[&account_id]).await.expect("original target graph").get(0);
            let work = Box::pin(async {
                managed = Box::pin(services.role().update(administrator.clone(), operation(), managed.id(), role::Update::new(managed.change_id(), role_input(10, &[])))).await.expect("actual permission removal");
                client.batch_execute("BEGIN").await.expect("direct unqualified establishment");
                let now = context.sample().await.expect("time");
                let error = client.execute("INSERT INTO identity.transfers(id,person_id,operator_id,revision_id,session_id,created_at,operation_id,hash,key_id) VALUES($1,$2,$3,$4,$5,$6,$7,decode(repeat('00',32),'hex'),0)", &[&Uuid::new_v4(), &candidate.identity().person_id(), &operator.revision_id(), &candidate.identity().revision_id(), &manager.session_id(), &now, &operation()]).await.expect_err("runtime SQL cannot skip current authority");
                assert_eq!(error.code(), Some(&SqlState::CHECK_VIOLATION));
                assert_eq!(error.as_db_error().expect("guard error").message(), "assisted transfer requires an authorized manager and a lower current target");
                client.batch_execute("ROLLBACK").await.expect("rejected writes ended");
                let error = services.transfer().complete_assisted(manager.clone(), account_id, completion(operation(), sent.id(), &code)).await.expect_err("the completion rechecks current management");
                assert_eq!(error.code(), Some("permission_denied"));
                Box::pin(services.role().update(administrator.clone(), operation(), managed.id(), role::Update::new(managed.change_id(), role_input(10, &["institution.accounts"])))).await.expect("restore actual permission");
                owner.client().batch_execute("CREATE FUNCTION public.fail_completion() RETURNS trigger LANGUAGE plpgsql SET search_path='' AS $$ BEGIN RAISE EXCEPTION 'injected completion write failure' USING ERRCODE='23514', CONSTRAINT='h2_completion_fault'; END $$").await.expect("bounded write failure fixture");
                for (table, action) in [("replacements", "INSERT"), ("handoffs", "DELETE"), ("completions", "INSERT")] {
                    owner.client().batch_execute(&format!("CREATE TRIGGER fail_completion BEFORE {action} ON identity.{table} FOR EACH ROW EXECUTE FUNCTION public.fail_completion()")).await.expect("install exact write fault");
                    let operation_id = operation();
                    let error = services.transfer().complete_assisted(manager.clone(), account_id, completion(operation_id, sent.id(), &code)).await.expect_err("a failed effect rolls back the complete operation");
                    let crate::kernel::Error::Unavailable(fault) = error else { panic!("expected a confirmed write failure at {table}: {error:?}"); };
                    let query = match fault.kind() {
                        crate::kernel::FaultKind::History(crate::history::Error::Query(query))
                        | crate::kernel::FaultKind::Transfer(crate::identity::transfer::Error::Query(query))
                        | crate::kernel::FaultKind::Query(query) => query,
                        other => panic!("expected the injected query failure at {table}: {other:?}"),
                    };
                    assert_eq!(query.0.code(), Some(&SqlState::CHECK_VIOLATION), "{table}");
                    assert_eq!(query.0.as_db_error().expect("injected database cause").constraint(), Some("h2_completion_fault"), "{table}");
                    owner.client().batch_execute(&format!("DROP TRIGGER fail_completion ON identity.{table}")).await.expect("remove exact write fault");
                    let after: String = client.query_one("SELECT jsonb_build_object('account',to_jsonb(account),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session WHERE account_id=account.id),'handoffs',(SELECT jsonb_agg(to_jsonb(handoff) ORDER BY handoff.id) FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=account.id))::text FROM identity.accounts account WHERE id=$1", &[&account_id]).await.expect("independent complete rollback").get(0);
                    assert_eq!(after, before, "{table}");
                    assert!(client.query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.replacements) AND NOT EXISTS(SELECT 1 FROM identity.revocations) AND NOT EXISTS(SELECT 1 FROM identity.completions WHERE family='mobile_completion' AND operation_id=$1) AND (SELECT failures=0 FROM identity.confirmations WHERE id=$2)", &[&operation_id, &sent.id()]).await.expect("no partial original or consumed guess").get::<_,bool>(0));
                    services.current().read(&target).await.expect("rolled-back target and session anchors remain current");
                    services.current().read(&manager).await.expect("manager remains current after rollback");
                    services.ready().await.expect("known rollback preserves admission");
                }
                owner.client().batch_execute("DROP FUNCTION public.fail_completion()").await.expect("fault fixture removed");
                services.transfer().complete_assisted(manager.clone(), account_id, completion(operation(), sent.id(), &code)).await.expect("same source succeeds with every required effect");
                services.current().read(&manager).await.expect("manager remains current");
                services.drain().await.expect("work drained"); context.stop().await.expect("context ended");
            });
            tokio::select! { biased; _=supplier.observe()=>panic!("SQL and completion never verify"), _=sms.request()=>panic!("SQL and completion never send"), ()=work=>{} }
            owner.terminate().await.expect("fault owner ended"); writer.terminate().await.expect("runtime writer ended"); sms.stop().await; supplier.stop().await;
        })).await.expect("bounded direct SQL protection");
    }));
}
