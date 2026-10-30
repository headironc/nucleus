#![allow(clippy::expect_used)]

use std::{sync::Arc, time::Duration};

use chrono::DateTime;
use fixture::database_proxy::{Fault, Proxy};
use serde_json::json;
use tokio::{runtime::Runtime, time::timeout};

use crate::{
    access::{
        appointment::tests::{register, register_numbered},
        role,
        role::tests::{actor, start},
    },
    account::tests::{assign, register_manager},
    center::affiliation::{self, Code, Error, Id, Input, Rejection, command, envelope},
    identity::{
        authentication::{self, Database, Supplier, server},
        current, person,
    },
    pagination::Pagination,
};

fn operation() -> crate::identity::operation::Id {
    uuid::Uuid::new_v4()
        .to_string()
        .parse()
        .expect("legacy operation")
}

async fn runtime_privilege(client: &tokio_postgres::Client, user: &str, statement: &str) {
    let sql: String = client
        .query_one("SELECT format($1::text, $2::text)", &[&statement, &user])
        .await
        .expect("quote isolated runtime role")
        .get(0);
    client
        .batch_execute(&sql)
        .await
        .expect("change only isolated runtime privilege");
}

async fn backend_ended(client: &tokio_postgres::Client, pid: i32) {
    timeout(Duration::from_secs(3), async {
        loop {
            if !client.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND datname=current_database())", &[&pid]).await.expect("original backend state").get::<_, bool>(0) {
                break;
            }
            tokio::task::yield_now().await;
        }
    }).await.expect("unconfirmed backend ended before reuse");
}

async fn create_role(
    services: &crate::startup::Services,
    actor: &current::Actor,
    name: &str,
    level: i16,
    permissions: &[&str],
) -> role::Role {
    Box::pin(
        services.role().create(
            actor.clone(),
            operation(),
            serde_json::from_value(json!({"name":name,"level":level,"permissions":permissions}))
                .expect("role input"),
        ),
    )
    .await
    .expect("role creation")
}

fn input(code: &str) -> Input {
    serde_json::from_value(json!({"affiliation": code})).expect("closed input")
}

fn correction(account: &current::Account, code: &str) -> affiliation::management::Input {
    serde_json::from_value(json!({
        "target": {"type": "account", "accountId": account.id(), "personId": account.person_id(), "revisionId": account.revision_id()},
        "affiliation": code,
    })).expect("confirmed target")
}

#[test]
fn strict_time_rejects_equal_and_backward_changes() {
    let now = DateTime::from_timestamp(1_700_000_000, 0).expect("fixed time");
    for latest in [now, now + chrono::Duration::microseconds(1)] {
        assert!(matches!(
            command::advancing::<Rejection>(Some(latest), now),
            Err(Error::Unavailable(_))
        ));
    }
    command::advancing::<Rejection>(Some(now - chrono::Duration::microseconds(1)), now)
        .expect("later time");
    command::advancing::<Rejection>(None, now).expect("first value");
}

#[test]
fn offline_input_rejects_missing_unknown_and_invalid_fields_without_echoing_private_input() {
    let id = uuid::Uuid::new_v4();
    let input = json!({"operator":{"mobile":"13800000000","accountId":id,"revisionId":id},"target":{"accountId":id,"personId":id,"revisionId":id,"affiliationId":id},"affiliation":"13"});
    serde_json::from_value::<affiliation::admin::Input>(input.clone())
        .expect("complete offline input");
    for (pointer, value) in [
        ("/operator/mobile", json!("invalid-private-mobile")),
        ("/operator/accountId", json!(null)),
        ("/operator/revisionId", json!("invalid-private-revision")),
        ("/target/accountId", json!(null)),
        ("/target/personId", json!("invalid-private-person")),
        ("/target/revisionId", json!(false)),
        ("/target/affiliationId", json!(null)),
        ("/affiliation", json!("invalid-private-affiliation")),
    ] {
        let mut changed = input.clone();
        *changed.pointer_mut(pointer).expect("existing field") = value;
        let error = serde_json::from_value::<affiliation::admin::Input>(changed)
            .err()
            .expect("invalid supplied field");
        assert!(!error.to_string().contains("invalid-private"));
    }
    for path in ["", "/operator", "/target"] {
        let mut changed = input.clone();
        changed
            .pointer_mut(path)
            .expect("input object")
            .as_object_mut()
            .expect("object")
            .insert("extra".into(), json!(true));
        assert!(serde_json::from_value::<affiliation::admin::Input>(changed).is_err());
    }
    for (path, field) in [
        ("", "affiliation"),
        ("/operator", "accountId"),
        ("/target", "affiliationId"),
    ] {
        let mut changed = input.clone();
        changed
            .pointer_mut(path)
            .expect("input object")
            .as_object_mut()
            .expect("object")
            .remove(field);
        assert!(serde_json::from_value::<affiliation::admin::Input>(changed).is_err());
    }
}

#[test]
fn envelope_retains_keys_and_binds_person_and_row() {
    let inventory = authentication::inventory(authentication::keys());
    let person_id = person::Id::allocate();
    let id = Id::allocate();
    let value =
        envelope::seal::<Rejection>(&inventory.encryption, person_id, id, Code::CommunistParty)
            .expect("envelope");
    assert!(
        envelope::open::<Rejection>(&inventory.encryption, person_id, id, &value)
            .expect("same binding")
            == Code::CommunistParty
    );
    for (person_id, id) in [(person::Id::allocate(), id), (person_id, Id::allocate())] {
        assert!(matches!(
            envelope::open::<Rejection>(&inventory.encryption, person_id, id, &value),
            Err(Error::Corrupt(_))
        ));
    }
}

#[test]
fn envelope_matches_python_aes_gcm_siv_vector_and_retained_root_versions() {
    use platform::cryptography::envelope::Envelope;
    use zeroize::Zeroizing;

    use crate::{
        database::sealed::Sealed,
        kernel::keys::Keys,
        keys::{self, Derived, Entry, Material},
    };

    let old_id = keys::Id::new(0).expect("old key");
    let new_id = keys::Id::new(1).expect("new key");
    let old = || {
        Entry::new(
            old_id,
            Material(Zeroizing::new(std::array::from_fn(|index| {
                u8::try_from(index).expect("32 bytes")
            }))),
        )
    };
    let rotated = Keys::new(Derived::fixed(
        new_id,
        vec![
            old(),
            Entry::new(new_id, Material(Zeroizing::new([99; 32]))),
        ],
    ));
    let missing = Keys::new(Derived::fixed(
        new_id,
        vec![Entry::new(new_id, Material(Zeroizing::new([99; 32])))],
    ));
    let person_id = "00000000-0000-4000-8000-000000000001"
        .parse()
        .expect("fixed person");
    let id =
        Id(uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000002").expect("fixed record"));
    // Python struct.pack('>I') and cryptography AESGCMSIV, with key 0..31 and nonce 0..11.
    let value = Sealed(
        Envelope::new(
            hex::decode(
                "010000000000000000000102030405060708090a0bcc444c0cdfecd927ab78243c8ea478c8d062",
            )
            .expect("independent vector"),
        )
        .expect("envelope layout"),
    );
    assert!(
        envelope::open::<Rejection>(&rotated, person_id, id, &value).expect("retained old key")
            == Code::CommunistParty
    );
    assert!(matches!(
        envelope::open::<Rejection>(&missing, person_id, id, &value),
        Err(Error::Corrupt(_))
    ));
    let changed = envelope::seal::<Rejection>(&rotated, person_id, id, Code::CommunistParty)
        .expect("technical reencryption");
    assert_eq!(changed.0.header().key_id(), new_id);
    assert!(
        envelope::open::<Rejection>(&rotated, person_id, id, &changed)
            .expect("same fact and binding")
            == Code::CommunistParty
    );
}

#[test]
fn concurrent_first_fills_commit_one_encrypted_fact_and_one_safe_audit() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(40), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(database.connections(&database.user), &supplier).await;
            let member = actor(&database, &services, &mut supplier).await;
            let person = services.current().read(&member).await.expect("current person");
            let owner = database.connections("postgres").recover().await.expect("isolated lock owner");
            let observer = database.connections("postgres").recover().await.expect("independent lock observer");
            owner.client().batch_execute("BEGIN").await.expect("begin synchronization");
            owner.client().query_one("SELECT id FROM identity.persons WHERE id=$1 FOR UPDATE", &[&person.person_id()]).await.expect("hold both fills before decision");
            let first = { let service = services.affiliation().clone(); let member = member.clone(); tokio::spawn(async move { service.create(member, input("01")).await }) };
            let second = { let service = services.affiliation().clone(); let member = member.clone(); tokio::spawn(async move { service.create(member, input("13")).await }) };
            timeout(Duration::from_secs(3), async {
                loop {
                    let blocked: i64 = observer.client().query_one("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND cardinality(pg_blocking_pids(pid))>0", &[]).await.expect("both requests actually wait").get(0);
                    if blocked >= 2 { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("both fills reached coordination");
            owner.client().batch_execute("ROLLBACK").await.expect("release first-fill decision");
            let results = [first.await.expect("first waiter"), second.await.expect("second waiter")];
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
            assert_eq!(results.iter().filter(|result| matches!(result, Err(Error::Rejected(Rejection::Conflict)))).count(), 1, "{results:?}");
            let row = owner.client().query_one("SELECT count(*), min(octet_length(value)), bool_and(operator_id=$2) FROM center.affiliations WHERE person_id=$1", &[&person.person_id(), &person.revision_id()]).await.expect("independent fact verification");
            assert_eq!(row.get::<_, i64>(0), 1);
            assert_eq!(row.get::<_, i32>(1), 39);
            assert!(row.get::<_, bool>(2));
            let audits = owner.client().query("SELECT actor_id, action, target_type, target_id, details::text FROM audit.events WHERE target_type='center.affiliation'", &[]).await.expect("safe success audit");
            assert_eq!(audits.len(), 1);
            assert_eq!(audits[0].get::<_, person::Id>(0), person.person_id());
            assert_eq!(audits[0].get::<_, String>(1), "fill");
            assert_eq!(audits[0].get::<_, String>(2), "center.affiliation");
            let fact_id: Id = owner.client().query_one("SELECT id FROM center.affiliations WHERE person_id=$1", &[&person.person_id()]).await.expect("unique fact target").get(0);
            assert_eq!(audits[0].get::<_, Id>(3), fact_id);
            assert_eq!(audits[0].get::<_, String>(4), "{}");
            assert_eq!(services.affiliation().history(&member, Pagination::new(99, 20).expect("out-of-range page")).await.expect("empty page retains count").count(), 1);
            owner.terminate().await.expect("owner ended");
            observer.terminate().await.expect("observer ended");
            services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        })).await.expect("bounded first-fill concurrency");
    }));
}

#[test]
fn cancelled_first_fill_ends_its_backend_without_a_fact_or_audit() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(30), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(database.connections(&database.user), &supplier).await;
            let member = actor(&database, &services, &mut supplier).await;
            let person = services.current().read(&member).await.expect("person");
            let owner = database.connections("postgres").recover().await.expect("lock owner");
            let observer = database.connections("postgres").recover().await.expect("independent backend observer");
            owner.client().batch_execute("BEGIN").await.expect("gate transaction");
            owner.client().query_one("SELECT id FROM identity.persons WHERE id=$1 FOR UPDATE", &[&person.person_id()]).await.expect("hold first-fill decision");
            let writer = {
                let service = services.affiliation().clone();
                let member = member.clone();
                tokio::spawn(async move { service.create(member, input("01")).await })
            };
            let pid = timeout(Duration::from_secs(3), async {
                loop {
                    if let Some(row) = observer.client().query_opt("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query='SELECT id FROM identity.persons WHERE id=$1 FOR UPDATE'", &[]).await.expect("write reached person lock") {
                        break row.get::<_, i32>(0);
                    }
                    tokio::task::yield_now().await;
                }
            }).await.expect("bounded write admission");
            writer.abort();
            assert!(writer.await.expect_err("caller cancelled the write").is_cancelled());
            owner.client().batch_execute("ROLLBACK").await.expect("release person gate");
            backend_ended(observer.client(), pid).await;
            let row = observer.client().query_one("SELECT (SELECT count(*) FROM center.affiliations), (SELECT count(*) FROM audit.events WHERE target_type='center.affiliation')", &[]).await.expect("cancellation leaves no durable write");
            assert_eq!(row.get::<_, i64>(0), 0);
            assert_eq!(row.get::<_, i64>(1), 0);
            services.affiliation().create(member, input("13")).await.expect("a new confirmation can fill once on a fresh lease");
            owner.terminate().await.expect("gate ended");
            observer.terminate().await.expect("observer ended");
            services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        })).await.expect("bounded cancelled first fill");
    }));
}

#[test]
fn equal_and_rewound_correction_times_preserve_all_facts_and_audits() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for (fault, equal) in [(Fault::EqualAffiliationTime, true), (Fault::RewoundCurrent, false)] {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let target = services.current().read(&member).await.expect("confirmed target");
                services.affiliation().create(member, input("01")).await.expect("latest effective time");
                let owner = database.connections("postgres").recover().await.expect("independent transaction observer");
                let snapshot = "SELECT jsonb_build_object('facts',(SELECT jsonb_agg(to_jsonb(fact) ORDER BY id) FROM center.affiliations fact),'audit',(SELECT jsonb_agg(to_jsonb(event) ORDER BY id) FROM audit.events event WHERE target_type='center.affiliation'))::text";
                let before: String = owner.client().query_one(snapshot, &[]).await.expect("original business and audit facts").get(0);
                proxy.arm(fault);
                let writer = {
                    let service = services.affiliation().clone();
                    let input = correction(&target, "13");
                    let person = target.person_id();
                    tokio::spawn(async move { service.correct(administrator, person, input).await })
                };
                let pid = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("decisive time query");
                proxy.release();
                let error = writer.await.expect("correction task").expect_err("time cannot admit a correction");
                if equal {
                    assert!(matches!(error, Error::Unavailable(fault) if matches!(fault.kind(), crate::kernel::FaultKind::Integrity(_))));
                    services.ready().await.expect("equal capability time does not corrupt retained facts");
                } else {
                    assert!(matches!(error, Error::Unavailable(fault) if matches!(fault.kind(), crate::kernel::FaultKind::Clock(crate::runtime::Error::ClockInvalid))));
                }
                backend_ended(owner.client(), pid).await;
                let after: String = owner.client().query_one(snapshot, &[]).await.expect("rejected transaction facts").get(0);
                assert_eq!(before, after, "neither business nor audit changed");
                owner.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await; supplier.stop().await;
            })).await.expect("bounded strict-time transaction");
        }
    }));
}

#[test]
fn reversed_management_attempts_lock_all_participants_before_qualification() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(45), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(database.connections(&database.user), &supplier).await;
            let administrator = actor(&database, &services, &mut supplier).await;
            let manager = Box::pin(register_manager(&database, &services, &mut supplier)).await;
            let member = register(&database, &services, &mut supplier).await;
            let higher = create_role(&services, &administrator, "Higher Corrector", 20, &["institution.affiliations.correct"]).await;
            let lower = create_role(&services, &administrator, "Lower Corrector", 10, &["institution.affiliations.correct"]).await;
            Box::pin(assign(&services, &administrator, "13700000000", &higher)).await;
            Box::pin(assign(&services, &administrator, "13800000000", &lower)).await;
            let own = services.current().read(&manager).await.expect("higher identity");
            let target = services.current().read(&member).await.expect("lower identity");
            let owner = database.connections("postgres").recover().await.expect("participant lock gate");
            let observer = database.connections("postgres").recover().await.expect("independent observer");
            owner.client().batch_execute("BEGIN").await.expect("gate transaction");
            owner.client().query("SELECT id FROM identity.persons WHERE id=ANY($1) ORDER BY id FOR UPDATE", &[&vec![own.person_id(), target.person_id()]]).await.expect("hold both participant rows");
            let forward = {
                let service = services.affiliation().clone();
                let manager = manager.clone();
                let input = correction(&target, "01");
                let person = target.person_id();
                tokio::spawn(async move { service.correct(manager, person, input).await })
            };
            let reverse = {
                let service = services.affiliation().clone();
                let member = member.clone();
                let input = correction(&own, "13");
                let person = own.person_id();
                tokio::spawn(async move { service.correct(member, person, input).await })
            };
            timeout(Duration::from_secs(3), async {
                loop {
                    let waiting: i64 = observer.client().query_one("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query='SELECT id FROM identity.persons WHERE id=$1 OR id=ANY($2) ORDER BY id FOR UPDATE'", &[]).await.expect("both reverse attempts reached participant locks").get(0);
                    if waiting == 2 { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("both opposite requests are waiting");
            owner.client().batch_execute("ROLLBACK").await.expect("release ordered participants");
            forward.await.expect("qualified task").expect("higher corrector succeeds");
            assert!(matches!(reverse.await.expect("reverse task"), Err(Error::Rejected(Rejection::Absent))));
            let row = owner.client().query_one("SELECT count(*), bool_and(person_id=$1) FROM center.affiliations", &[&target.person_id()]).await.expect("only the qualified target changed");
            assert_eq!(row.get::<_, i64>(0), 1);
            assert!(row.get::<_, bool>(1));
            assert_eq!(owner.client().query_one("SELECT count(*) FROM audit.events WHERE target_type='center.affiliation' AND action='correct' AND actor_id=$1", &[&own.person_id()]).await.expect("only the qualified correction audited").get::<_, i64>(0), 1);
            services.ready().await.expect("rejected reverse request preserves admission");
            owner.terminate().await.expect("gate ended");
            observer.terminate().await.expect("observer ended");
            services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        })).await.expect("bounded reversed management concurrency");
    }));
}

#[test]
fn managed_corrections_append_same_values_and_cancelled_people_keep_history() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(45), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(database.connections(&database.user), &supplier).await;
            let administrator = actor(&database, &services, &mut supplier).await;
            let operator = services.current().read(&administrator).await.expect("operator");
            let member = register(&database, &services, &mut supplier).await;
            let target = services.current().read(&member).await.expect("target");
            for _ in 0..2 {
                services.affiliation().correct(administrator.clone(), target.person_id(), correction(&target, "01")).await.expect("each current correction appends");
            }
            let page = services.affiliation().history_by_person(&administrator, target.person_id(), Pagination::new(1, 1).expect("one item")).await.expect("management projection");
            assert_eq!(page.count(), 2);
            assert_eq!(page.affiliations().len(), 1);
            assert_eq!(page.affiliations()[0].operator().revision_id(), operator.revision_id());
            assert!(matches!(services.affiliation().create(member.clone(), input("13")).await, Err(Error::Rejected(Rejection::Conflict))));
            let operation = uuid::Uuid::new_v4().to_string().parse().expect("diagnostic operation");
            Box::pin(services.cancellation().cancel(member.clone(), operation, serde_json::from_value(json!({"accountId": target.id(), "personId": target.person_id(), "revisionId": target.revision_id()})).expect("cancellation"))).await.expect("cancel actual account");
            assert!(matches!(services.affiliation().correct(administrator.clone(), target.person_id(), correction(&target, "13")).await, Err(Error::Rejected(Rejection::TargetChanged))));
            services.affiliation().correct(administrator.clone(), target.person_id(), serde_json::from_value(json!({"target": {"type": "person", "personId": target.person_id(), "revisionId": target.revision_id()}, "affiliation": "13"})).expect("permanent person confirmation")).await.expect("correct cancelled person");
            let replacement = register(&database, &services, &mut supplier).await;
            assert!(matches!(services.affiliation().read(&replacement).await, Err(Error::Rejected(Rejection::Unset))));
            assert!(matches!(services.affiliation().read(&member).await, Err(Error::Unauthenticated)));
            let history = services.affiliation().history_by_person(&administrator, target.person_id(), Pagination::new(1, 20).expect("full page")).await.expect("permanent retained history");
            assert_eq!(history.count(), 3);
            assert!(history.affiliations()[0].value() == Code::Unaffiliated);
            assert!(history.affiliations().windows(2).all(|pair| pair[0].created_at() > pair[1].created_at()));
            services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        })).await.expect("bounded managed history lifecycle");
    }));
}

#[test]
fn audit_failure_rolls_back_value_and_unknown_commit_is_only_observed_by_current_state() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(
            Duration::from_secs(45),
            Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) =
                    start(authentication::proxied(&database, &proxy), &supplier).await;
                let member = actor(&database, &services, &mut supplier).await;
                let person = services
                    .current()
                    .read(&member)
                    .await
                    .expect("current person");
                let owner = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("isolated permissions owner");
                runtime_privilege(
                    owner.client(),
                    &database.user,
                    "REVOKE INSERT ON audit.events FROM %I",
                )
                .await;
                assert!(matches!(
                    services
                        .affiliation()
                        .create(member.clone(), input("01"))
                        .await,
                    Err(Error::Unavailable(_))
                ));
                assert_eq!(
                    owner
                        .client()
                        .query_one("SELECT count(*) FROM center.affiliations", &[])
                        .await
                        .expect("audit and fact atomicity")
                        .get::<_, i64>(0),
                    0
                );
                runtime_privilege(
                    owner.client(),
                    &database.user,
                    "GRANT INSERT ON audit.events TO %I",
                )
                .await;
                proxy.arm(Fault::LostCommit);
                assert!(matches!(
                    services
                        .affiliation()
                        .create(member.clone(), input("13"))
                        .await,
                    Err(Error::Unknown(_))
                ));
                let pid = timeout(Duration::from_secs(3), proxy.intercepted())
                    .await
                    .expect("original commit interception");
                proxy.release();
                backend_ended(owner.client(), pid).await;
                assert!(
                    services
                        .affiliation()
                        .read(&member)
                        .await
                        .expect("ordinary current state after unknown")
                        .value
                        == Code::Unaffiliated
                );
                assert!(matches!(
                    services.affiliation().create(member, input("13")).await,
                    Err(Error::Rejected(Rejection::Conflict))
                ));
                assert_eq!(
                    owner
                        .client()
                        .query_one(
                            "SELECT count(*) FROM center.affiliations WHERE person_id=$1",
                            &[&person.person_id()]
                        )
                        .await
                        .expect("no automatic rewrite or replay")
                        .get::<_, i64>(0),
                    1
                );
                owner.terminate().await.expect("owner ended");
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                proxy.stop().await;
                supplier.stop().await;
            }),
        )
        .await
        .expect("bounded atomic audit and unknown outcome");
    }));
}

#[test]
fn runtime_keeps_history_append_only_and_startup_rejects_unreadable_retained_envelopes() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for damage in [
            "UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38) # 1)",
            "UPDATE center.affiliations SET value=set_byte(value,8,99)",
            "UPDATE center.affiliations SET id=gen_random_uuid()",
        ] {
            timeout(
                Duration::from_secs(35),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let member = actor(&database, &services, &mut supplier).await;
                    services
                        .affiliation()
                        .create(member, input("01"))
                        .await
                        .expect("retained fact");
                    let runtime = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("actual runtime identity");
                    for sql in [
                        "UPDATE center.affiliations SET value=value",
                        "DELETE FROM center.affiliations",
                        "TRUNCATE center.affiliations",
                    ] {
                        let error = runtime
                            .client()
                            .batch_execute(sql)
                            .await
                            .expect_err("runtime cannot modify retained history");
                        assert_eq!(
                            error.code().map(tokio_postgres::error::SqlState::code),
                            Some("42501")
                        );
                    }
                    runtime.terminate().await.expect("runtime observer ended");
                    services.close();
                    services
                        .drain()
                        .await
                        .expect("drained before isolated damage");
                    context.stop().await.expect("stopped");
                    let owner = database
                        .connections("postgres")
                        .recover()
                        .await
                        .expect("isolated corruption owner");
                    owner
                        .client()
                        .batch_execute(damage)
                        .await
                        .expect("corrupt only this disposable test database");
                    owner.terminate().await.expect("owner ended");
                    let context =
                        crate::runtime::Context::new(database.connections(&database.user))
                            .await
                            .expect("restart context");
                    let result = Box::pin(crate::startup::Services::start(
                        crate::database::Barrier::begin(context.clone())
                            .await
                            .expect("restart barrier"),
                        authentication::inventory(authentication::keys()),
                        crate::startup::Settings::new(
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
                    .await;
                    assert!(matches!(
                        result,
                        Err(crate::startup::Error::Affiliation(Error::Corrupt(_)))
                    ));
                    context.stop().await.expect("failed startup stopped");
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded retained envelope startup check");
        }
    }));
}

#[test]
fn management_keeps_exact_permissions_levels_and_candidates_independent_of_values() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(70), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(database.connections(&database.user), &supplier).await;
            let administrator = actor(&database, &services, &mut supplier).await;
            let manager = Box::pin(register_manager(&database, &services, &mut supplier)).await;
            let member = register(&database, &services, &mut supplier).await;
            let target = services.current().read(&member).await.expect("target");
            let own = services.current().read(&manager).await.expect("manager");
            let system = services.current().read(&administrator).await.expect("system");
            let mut selected = create_role(&services, &administrator, "Corrector", 20, &[]).await;
            Box::pin(assign(&services, &administrator, "13700000000", &selected)).await;
            let lower = create_role(&services, &administrator, "Lower", 10, &[]).await;
            let equal = create_role(&services, &administrator, "Equal", 20, &[]).await;
            let higher = create_role(&services, &administrator, "Higher", 30, &[]).await;
            let owner = database.connections("postgres").recover().await.expect("isolated observer");
            let mut count = 0_i64;
            for permissions in [vec!["institution.accounts"], vec![], vec!["institution.affiliations.read"], vec!["institution.affiliations.correct", "institution.affiliations.read"], vec!["institution.affiliations.correct"]] {
                selected = Box::pin(services.role().update(administrator.clone(), operation(), selected.id(), serde_json::from_value(json!({"changeId":selected.change_id(),"name":"Corrector","level":20,"permissions":permissions})).expect("permission update"))).await.expect("actual permission change");
                let can_correct = permissions.contains(&"institution.affiliations.correct");
                for target_role in [&lower, &equal, &higher] {
                    Box::pin(assign(&services, &administrator, "13800000000", target_role)).await;
                    let result = services.affiliation().correct(manager.clone(), target.person_id(), correction(&target, "01")).await;
                    if can_correct && target_role.level() < selected.level() {
                        result.expect("strictly lower target"); count += 1;
                    } else {
                        assert_eq!(result.expect_err("no inferred write qualification").code(), if can_correct { Some("target_unavailable") } else { Some("permission_denied") });
                    }
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM center.affiliations", &[]).await.expect("only qualified appends").get::<_, i64>(0), count);
                }
            }
            for unavailable in [&own, &system] {
                assert!(matches!(services.affiliation().correct(manager.clone(), unavailable.person_id(), correction(unavailable, "13")).await, Err(Error::Rejected(Rejection::Absent))));
            }
            Box::pin(assign(&services, &administrator, "13800000000", &lower)).await;
            let before = services.affiliation().candidate_by_person(&manager, target.person_id()).await.expect("candidate without value disclosure");
            runtime_privilege(owner.client(), &database.user, "REVOKE SELECT ON center.affiliations FROM %I").await;
            let after = services.affiliation().candidate_by_person(&manager, target.person_id()).await.expect("candidate does not read value table");
            assert_eq!(before.target(), after.target());
            assert_eq!(before.identity().revision_id(), after.identity().revision_id());
            assert_eq!(services.affiliation().read_person(&manager, target.person_id()).await.err().expect("correction permission cannot read").code(), Some("permission_denied"));
            let damaged_through: DateTime<chrono::Utc> = owner.client().query_one("SELECT max(created_at) FROM center.affiliations", &[]).await.expect("exact damage scope").get(0);
            runtime_privilege(owner.client(), &database.user, "GRANT SELECT ON center.affiliations TO %I").await;
            owner.client().batch_execute("UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38)#1)").await.expect("damage retained tags");
            services.affiliation().candidate_by_mobile(&manager, "13800000000".parse().expect("mobile")).await.expect("candidate ignores unreadable value");
            for code in 1..=13 {
                assert_eq!(services.affiliation().read_person(&manager, target.person_id()).await.err().expect("no value oracle").code(), Some("permission_denied"));
                services.affiliation().correct(manager.clone(), target.person_id(), correction(&target, &format!("{code:02}"))).await.expect("every code produces the same empty success");
            }
            count += 13;
            owner.client().execute("UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38)#1) WHERE created_at<=$1", &[&damaged_through]).await.expect("restore exactly the original damaged test tags");
            let selected = services.correction().candidate_by_mobile(&administrator, "13700000000".parse().expect("operator mobile")).await.expect("real-name correction target");
            Box::pin(services.correction().correct(administrator.clone(), operation(), serde_json::from_value(json!({"target":selected.target(),"name":"Revised Operator","idNumber":"000000200002290029"})).expect("real-name input"))).await.expect("operator receives a new real-name version");
            let retained = services.affiliation().history_by_person(&administrator, target.person_id(), Pagination::new(1,100).expect("full retained page")).await.expect("original operators after real-name change");
            assert!(retained.affiliations().iter().all(|item| item.operator().revision_id() == own.revision_id()), "later real-name changes do not rewrite original operator snapshots");
            let own = services.current().read(&manager).await.expect("new operator identity");
            Box::pin(services.suspension().suspend(administrator.clone(), operation(), target.id(), serde_json::from_value(json!({"personId":target.person_id(),"revisionId":target.revision_id(),"mobile":"13800000000"})).expect("target suspension"))).await.expect("actual suspended target");
            services.affiliation().correct(manager.clone(), target.person_id(), correction(&target, "13")).await.expect("target suspension does not cancel management qualification");
            count += 1;
            Box::pin(services.suspension().suspend(administrator, operation(), own.id(), serde_json::from_value(json!({"personId":own.person_id(),"revisionId":own.revision_id(),"mobile":"13700000000"})).expect("manager suspension"))).await.expect("actual manager suspension");
            assert_eq!(services.affiliation().correct(manager, target.person_id(), correction(&target, "13")).await.expect_err("suspended operator cannot continue").code(), Some("authentication_invalid"));
            assert_eq!(owner.client().query_one("SELECT count(*) FROM center.affiliations", &[]).await.expect("refusals preserve facts").get::<_, i64>(0), count);
            owner.terminate().await.expect("owner ended");
            services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        })).await.expect("bounded management qualification matrix");
    }));
}

#[test]
fn read_timeouts_cancellation_and_unconfirmed_read_commit_withhold_results_and_end_backends() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for (fault, history, cancel) in [
            (Fault::StalledAffiliationRead, false, false),
            (Fault::StalledAffiliationCount, true, false),
            (Fault::StalledAffiliationPage, true, false),
            (Fault::LostReadCommit, false, false),
            (Fault::StalledAffiliationRead, false, true),
        ] {
            timeout(
                Duration::from_secs(35),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) =
                        start(authentication::proxied(&database, &proxy), &supplier).await;
                    let member = actor(&database, &services, &mut supplier).await;
                    services
                        .affiliation()
                        .create(member.clone(), input("01"))
                        .await
                        .expect("readable current fact");
                    proxy.arm(fault);
                    let service = services.affiliation().clone();
                    let selected = member.clone();
                    let waiter = tokio::spawn(async move {
                        if history {
                            service
                                .history(&selected, Pagination::new(1, 20).expect("page"))
                                .await
                                .map(|_| ())
                        } else {
                            service.read(&selected).await.map(|_| ())
                        }
                    });
                    let pid = timeout(Duration::from_secs(3), proxy.intercepted())
                        .await
                        .expect("actual read boundary");
                    if cancel {
                        waiter.abort();
                        assert!(waiter.await.expect_err("caller cancelled").is_cancelled());
                    } else {
                        assert!(matches!(
                            timeout(Duration::from_secs(5), waiter)
                                .await
                                .expect("read budget")
                                .expect("read waiter"),
                            Err(Error::Unavailable(_))
                        ));
                    }
                    proxy.release();
                    let owner = database
                        .connections("postgres")
                        .recover()
                        .await
                        .expect("independent backend observer");
                    backend_ended(owner.client(), pid).await;
                    services
                        .ready()
                        .await
                        .expect("ordinary dependency failures do not close admission");
                    assert!(
                        services
                            .affiliation()
                            .read(&member)
                            .await
                            .expect("fresh connection reads current state")
                            .value
                            == Code::CommunistParty
                    );
                    owner.terminate().await.expect("owner ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    proxy.stop().await;
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded read failure lifecycle");
        }
    }));
}

#[test]
fn history_count_and_page_keep_one_snapshot_while_a_new_correction_commits() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(
            Duration::from_secs(40),
            Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) =
                    start(authentication::proxied(&database, &proxy), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let member = register(&database, &services, &mut supplier).await;
                let target = services.current().read(&member).await.expect("target");
                services
                    .affiliation()
                    .create(member.clone(), input("01"))
                    .await
                    .expect("first value");
                proxy.arm(Fault::StalledAffiliationPage);
                let service = services.affiliation().clone();
                let selected = member.clone();
                let waiter = tokio::spawn(async move {
                    service
                        .history(&selected, Pagination::new(1, 20).expect("page"))
                        .await
                });
                timeout(Duration::from_secs(3), proxy.intercepted())
                    .await
                    .expect("page paused after count");
                services
                    .affiliation()
                    .correct(administrator, target.person_id(), correction(&target, "13"))
                    .await
                    .expect("new correction commits beside read-only snapshot");
                proxy.release();
                let history = waiter
                    .await
                    .expect("snapshot waiter")
                    .expect("complete original snapshot");
                assert_eq!(history.count(), 1);
                assert_eq!(history.affiliations().len(), 1);
                assert!(history.affiliations()[0].value == Code::CommunistParty);
                let latest = services
                    .affiliation()
                    .history(&member, Pagination::new(1, 20).expect("page"))
                    .await
                    .expect("next snapshot");
                assert_eq!(latest.count(), 2);
                assert_eq!(latest.affiliations().len(), 2);
                assert!(latest.affiliations()[0].value == Code::Unaffiliated);
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                proxy.stop().await;
                supplier.stop().await;
            }),
        )
        .await
        .expect("bounded history snapshot interleaving");
    }));
}

#[test]
fn offline_correction_rechecks_operators_system_targets_and_expected_current_record() {
    let server = server();
    std::thread::Builder::new().stack_size(32 * 1024 * 1024).spawn(move || {
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["ordinary_operator", "self_operator", "non_system", "unset", "same_value", "operator_suspended", "operator_cancelled", "operator_account", "operator_revision", "operator_mobile", "target_suspended", "target_account", "target_revision", "expected_record", "dry_run", "unknown"] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let administrator = actor(&database, &services, &mut supplier).await;
                    let operator = register(&database, &services, &mut supplier).await;
                    let target_actor = Box::pin(register_numbered(&database, &services, &mut supplier, 2)).await;
                    let target = services.current().read(&target_actor).await.expect("target before system appointment");
                    let normal = services.current().read(&operator).await.expect("normal operator");
                    if case != "unset" {
                        services.affiliation().correct(administrator.clone(), target.person_id(), correction(&target, "01")).await.expect("initial target value");
                    }
                    if case == "target_suspended" {
                        Box::pin(services.suspension().suspend(administrator.clone(), operation(), target.id(), serde_json::from_value(json!({"personId":target.person_id(),"revisionId":target.revision_id(),"mobile":"13800000002"})).expect("target suspension"))).await.expect("suspend before system appointment");
                    }
                    if case != "non_system" {
                        Box::pin(crate::access::appoint::run(services.app().clone(), Arc::new(authentication::inventory(authentication::keys()).identity), operation(), serde_json::from_value(json!({"operator":{"mobile":"13900000000"},"accountId":target.id(),"revisionId":target.revision_id(),"selection":{"action":"grant"}})).expect("system appointment"), false)).await.expect("actual system role grant");
                    }
                    if case == "operator_suspended" {
                        Box::pin(services.suspension().suspend(administrator.clone(), operation(), normal.id(), serde_json::from_value(json!({"personId":normal.person_id(),"revisionId":normal.revision_id(),"mobile":"13800000000"})).expect("operator suspension"))).await.expect("suspend normal operator");
                    }
                    if case == "operator_cancelled" {
                        Box::pin(services.cancellation().cancel(operator, operation(), serde_json::from_value(json!({"accountId":normal.id(),"personId":normal.person_id(),"revisionId":normal.revision_id()})).expect("operator cancellation"))).await.expect("cancel normal operator");
                    }
                    let owner = database.connections(&database.user).recover().await.expect("independent retained facts");
                    let expected = owner.client().query_opt("SELECT id FROM center.affiliations WHERE person_id=$1 ORDER BY created_at DESC LIMIT 1", &[&target.person_id()]).await.expect("current record").map(|row| row.get::<_, Id>(0)).unwrap_or_else(Id::allocate);
                    let (operator_mobile, operator_account, operator_revision) = if case == "self_operator" { ("13800000002", target.id(), target.revision_id()) } else { ("13800000000", normal.id(), normal.revision_id()) };
                    let mut input = json!({"operator":{"mobile":operator_mobile,"accountId":operator_account,"revisionId":operator_revision},"target":{"accountId":target.id(),"personId":target.person_id(),"revisionId":target.revision_id(),"affiliationId":expected},"affiliation":if case=="same_value" {"01"} else {"13"}});
                    match case {
                        "operator_account" => input["operator"]["accountId"] = json!(uuid::Uuid::new_v4()),
                        "operator_revision" => input["operator"]["revisionId"] = json!(uuid::Uuid::new_v4()),
                        "operator_mobile" => input["operator"]["mobile"] = json!("13800000999"),
                        "target_account" => input["target"]["accountId"] = json!(uuid::Uuid::new_v4()),
                        "target_revision" => input["target"]["revisionId"] = json!(uuid::Uuid::new_v4()),
                        "expected_record" => input["target"]["affiliationId"] = json!(uuid::Uuid::new_v4()),
                        _ => {}
                    }
                    let before: i64 = owner.client().query_one("SELECT count(*) FROM center.affiliations", &[]).await.expect("before facts").get(0);
                    let audit_before: i64 = owner.client().query_one("SELECT count(*) FROM audit.events WHERE target_type='center.affiliation'", &[]).await.expect("before audits").get(0);
                    services.close(); services.drain().await.expect("online work drained"); context.stop().await.expect("online context stopped");
                    let proxy = Proxy::start(&server).await;
                    let maintenance = crate::runtime::Context::hold(authentication::proxied(&database, &proxy)).await.expect("exclusive maintenance instance");
                    let app = crate::startup::Administration::start(crate::database::Barrier::begin(maintenance.clone()).await.expect("maintenance barrier"), authentication::inventory(authentication::keys()), authentication::session_settings().app_id().clone()).await.expect("verified offline application");
                    if case == "unknown" { proxy.arm(Fault::LostCommit); }
                    let result = app.correct_affiliation(serde_json::from_value(input.clone()).expect("well-shaped offline input"), case == "dry_run").await;
                    let success = matches!(case, "ordinary_operator" | "self_operator" | "target_suspended");
                    if success { assert!(result.expect("qualified system correction").is_some()); }
                    else if case == "dry_run" { assert!(result.expect("dry-run checks current qualification").is_none()); }
                    else if case == "unknown" {
                        assert!(matches!(result, Err(crate::kernel::Error::Unknown(_))));
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original offline commit lost its response");
                        proxy.release();
                        timeout(Duration::from_secs(3), async {
                            loop {
                                if owner.client().query_one("SELECT count(*) FROM center.affiliations", &[]).await.expect("observe current facts after unknown").get::<_, i64>(0) == before + 1 { break; }
                                tokio::task::yield_now().await;
                            }
                        }).await.expect("original offline backend committed");
                        assert_eq!(app.correct_affiliation(serde_json::from_value(input).expect("same stale confirmation"), false).await.expect_err("unknown is not replayed").code(), Some("target_changed"));
                    } else {
                        let expected = match case {
                            "non_system" => "target_unavailable",
                            "unset" => "affiliation_unset",
                            "same_value" => "state_conflict",
                            "operator_suspended" | "operator_cancelled" | "operator_account" | "operator_revision" | "operator_mobile" => "operator_unavailable",
                            _ => "target_changed",
                        };
                        assert_eq!(result.expect_err("current qualification rejects stale or unavailable input").code(), Some(expected), "{case}");
                    }
                    let added = i64::from(success || case == "unknown");
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM center.affiliations", &[]).await.expect("after facts").get::<_, i64>(0), before + added, "{case}");
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM audit.events WHERE target_type='center.affiliation'", &[]).await.expect("after audits").get::<_, i64>(0), audit_before + added, "{case}");
                    app.close(); app.drain().await.expect("maintenance drained"); maintenance.stop().await.expect("maintenance stopped");
                    owner.terminate().await.expect("observer ended"); proxy.stop().await; supplier.stop().await;
                })).await.expect("bounded offline qualification case");
            }
        }));
    }).expect("offline test thread").join().expect("offline test result");
}

#[test]
fn queued_reads_recheck_expiry_clock_and_global_admission_after_pool_wait() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        for case in ["expired", "rewound", "closed"] {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                let member = actor(&database, &services, &mut supplier).await;
                services.affiliation().create(member.clone(), input("01")).await.expect("first value");
                let owner = database.connections("postgres").recover().await.expect("time observer");
                let expires_at: DateTime<chrono::Utc> = owner.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1", &[&member.session_id()]).await.expect("actual session expiry").get(0);
                let first = context.connections().lease().await.expect("first pool slot");
                let second = context.connections().lease().await.expect("second pool slot");
                let baseline = context.now(first.client()).await.expect("initial high water");
                assert!(expires_at > baseline);
                let service = services.affiliation().clone();
                let (waiting, queued) = tokio::sync::oneshot::channel();
                let reader = tokio::spawn(async move {
                    use std::future::Future as _;
                    let mut result = Box::pin(service.read(&member));
                    std::future::poll_fn(|cx| {
                        assert!(result.as_mut().poll(cx).is_pending(), "both pool slots force a real wait");
                        std::task::Poll::Ready(())
                    }).await;
                    waiting.send(()).expect("read poll observed");
                    result.await
                });
                timeout(Duration::from_secs(2), queued).await.expect("reader reached pool wait").expect("reader notified");
                match case {
                    "expired" => {
                        let elapsed = expires_at - database.now().await + chrono::TimeDelta::seconds(1);
                        crate::registration::tests::age_facts(&database, &services, elapsed).await;
                    }
                    "rewound" => proxy.arm(Fault::RewoundCurrent),
                    "closed" => services.close(),
                    _ => unreachable!(),
                }
                first.release(); second.release();
                if case == "rewound" {
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("decisive current identity snapshot");
                    proxy.release();
                }
                let result = timeout(Duration::from_secs(3), reader).await.expect("bounded reader").expect("reader task");
                match case {
                    "expired" => {
                        assert!(matches!(result, Err(Error::Unauthenticated)));
                        services.ready().await.expect("normal session expiry retains admission");
                    }
                    "rewound" => assert!(matches!(result, Err(Error::Unavailable(fault)) if matches!(fault.kind(), crate::kernel::FaultKind::Clock(crate::runtime::Error::ClockInvalid)))),
                    "closed" => assert!(result.err().expect("queued read respects global closure").is_closed()),
                    _ => unreachable!(),
                }
                owner.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await; supplier.stop().await;
            }).await.expect("bounded queue and identity recheck");
        }
    });
}

#[test]
fn identity_precedes_sensitive_decoding_and_corruption_closes_admission_while_query_denial_does_not()
 {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for case in ["query", "tag", "key", "future", "operator", "expired"] {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let member = actor(&database, &services, &mut supplier).await;
                services.affiliation().create(member.clone(), input("01")).await.expect("retained current fact");
                let owner = database.connections("postgres").recover().await.expect("isolated fault owner");
                if case == "expired" {
                    let expires_at: DateTime<chrono::Utc> = owner.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1", &[&member.session_id()]).await.expect("session expiry").get(0);
                    crate::registration::tests::age_facts(&database, &services, expires_at - database.now().await + chrono::TimeDelta::seconds(1)).await;
                }
                if case == "query" {
                    runtime_privilege(owner.client(), &database.user, "REVOKE SELECT ON center.affiliations FROM %I").await;
                } else {
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated retained-fact damage");
                    let sql = match case {
                        "tag" | "expired" => "UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38)#1)",
                        "key" => "UPDATE center.affiliations SET value=set_byte(value,8,99)",
                        "future" => "UPDATE center.affiliations SET created_at=created_at+interval '1 day'",
                        "operator" => "UPDATE center.affiliations SET operator_id=gen_random_uuid()",
                        _ => unreachable!(),
                    };
                    owner.client().batch_execute(sql).await.expect("damage only the disposable database");
                    owner.client().batch_execute("COMMIT").await.expect("damage visible");
                }
                let error = services.affiliation().read(&member).await.err().expect("bad dependency or lost identity withholds value");
                match case {
                    "query" => {
                        assert!(matches!(error, Error::Unavailable(_)));
                        services.ready().await.expect("query rejection keeps admission open");
                        runtime_privilege(owner.client(), &database.user, "GRANT SELECT ON center.affiliations TO %I").await;
                        services.affiliation().read(&member).await.expect("normal read after dependency repair");
                    }
                    "expired" => {
                        assert!(matches!(error, Error::Unauthenticated));
                        services.ready().await.expect("expired identity did not inspect damaged private value");
                    }
                    _ => {
                        assert!(error.is_corrupt(), "{case}: {error:?}");
                        assert!(services.ready().await.is_err(), "detected damaged retained facts close admission");
                    }
                }
                owner.terminate().await.expect("owner ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            })).await.expect("bounded identity and corruption distinction");
        }
    }));
}
