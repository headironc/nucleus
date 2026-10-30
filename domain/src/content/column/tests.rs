#![allow(clippy::expect_used)]

use std::{sync::Arc, time::Duration};

use serde_json::json;
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use super::{Creation, Description, Error, Key, Name, Rejection};
use crate::{
    access::{
        appointment::tests::register,
        role::tests::{actor, start},
    },
    identity::{
        authentication::{self, Database, Supplier, server},
        operation,
    },
};

fn operation() -> operation::Id {
    Uuid::new_v4().to_string().parse().expect("operation")
}
fn input(key: &str, name: &str, enabled: bool) -> Creation {
    serde_json::from_value(json!({"key":key,"name":name,"description":"","enabled":enabled}))
        .expect("creation")
}

#[test]
fn controlled_values_normalize_text_and_reject_ambiguous_or_invalid_input() {
    for key in ["ab", "a-", "a0", &"a".repeat(32)] {
        assert!(key.parse::<Key>().is_ok());
    }
    for key in ["", "a", "Ab", "1a", " a", "a_", "新闻", &"a".repeat(33)] {
        assert!(key.parse::<Key>().is_err());
    }
    assert_eq!(
        "\u{2003}Cafe\u{301}　"
            .parse::<Name>()
            .expect("NFC name")
            .as_str(),
        "Café"
    );
    assert!("界".repeat(20).parse::<Name>().is_ok());
    assert!("界".repeat(21).parse::<Name>().is_err());
    assert!("界".repeat(100).parse::<Description>().is_ok());
    assert!("界".repeat(101).parse::<Description>().is_err());
    assert_eq!(
        "　"
            .parse::<Description>()
            .expect("empty description")
            .as_str(),
        ""
    );
    for name in ["", "　", "\nname", "name\u{85}", "a\0b"] {
        assert!(name.parse::<Name>().is_err());
    }
    for raw in [
        r#"{"key":"ab","name":"Name","description":"","enabled":true,"index":0}"#,
        r#"{"key":"ab","key":"cd","name":"Name","description":"","enabled":true}"#,
        r#"{"key":"ab","name":"Name","enabled":true}"#,
        r#"{"key":"ab","name":"Name","description":"","enabled":null}"#,
    ] {
        assert!(serde_json::from_str::<Creation>(raw).is_err());
    }
    let creation = input("news", " Cafe\u{301} ", false);
    assert_eq!(
        serde_json::to_string(&creation).expect("canonical input"),
        r#"{"key":"news","name":"Café","description":"","enabled":false}"#
    );
    let digest = creation
        .digest()
        .expect("digest")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    // Independently calculated over the exact UTF-8 canonical object, without length framing.
    assert_eq!(
        digest,
        "97d016ac23ce8a67af78074f6fc076e6d3c791de45a54104809eaa3b1e30fc4f"
    );
}

#[test]
fn creation_retains_original_results_across_append_and_restart() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(40), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(database.connections(&database.user), &supplier).await;
            let administrator = actor(&database, &services, &mut supplier).await;
            let member = register(&database, &services, &mut supplier).await;
            let service = services.column();
            assert!(matches!(service.create(member, operation(), input("news", "News", true)).await, Err(Error::Denied)));
            let original = operation();
            let first = service.create(administrator.clone(), original, input("news", " News ", false)).await.expect("create");
            let second = service.create(administrator.clone(), operation(), input("museum", "Museum", true)).await.expect("append");
            let replay = service.create(administrator.clone(), original, input("news", "News", false)).await.expect("normalized replay");
            assert_eq!(replay.id(), first.id());
            assert_eq!(replay.created_at(), first.created_at());
            assert_eq!(replay.column(), first.column());
            assert!(matches!(service.create(administrator.clone(), original, input("news", "Changed", false)).await, Err(Error::Mismatch)));
            assert!(matches!(service.create(administrator.clone(), operation(), input("news", "Other", true)).await, Err(Error::Rejected(Rejection::KeyInUse))));
            assert!(matches!(service.create(administrator.clone(), operation(), input("other", " News ", true)).await, Err(Error::Rejected(Rejection::NameInUse))));
            assert_eq!(first.column().index, 0);
            assert!(!first.column().enabled);
            assert_eq!(first.column().name.as_str(), "News");
            assert_eq!(second.column().index, 1);
            assert!(second.column().enabled);
            assert_ne!(first.column().id(), second.column().id());
            let observer = database.connections(&database.user).recover().await.expect("observer");
            let counts = observer.client().query_one("SELECT (SELECT count(*) FROM content.columns), (SELECT count(*) FROM content.changes), (SELECT count(*) FROM content.appends), (SELECT count(*) FROM content.completions)", &[]).await.expect("independent writes");
            for index in 0..4 { assert_eq!(counts.get::<_, i64>(index), 2); }
            for sql in ["DELETE FROM content.changes", "DELETE FROM content.columns", "UPDATE content.columns SET key='replacement'", "DELETE FROM content.completions"] {
                assert_eq!(observer.client().batch_execute(sql).await.expect_err("runtime immutability").code(), Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE));
            }
            observer.terminate().await.expect("observer ended");
            services.revocation().revoke(administrator.clone(), operation(), crate::identity::revocation::Scope::Current).await.expect("revoke");
            assert!(matches!(service.create(administrator.clone(), original, input("news", "News", false)).await, Err(Error::Unauthenticated)));
            services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
            let (context, services) = start(database.connections(&database.user), &supplier).await;
            let administrator = actor(&database, &services, &mut supplier).await;
            let restarted = services.column().create(administrator, original, input("news", "News", false)).await.expect("verified original after restart");
            assert_eq!(restarted.id(), first.id());
            assert_eq!(restarted.column(), first.column());
            assert_eq!(restarted.created_at(), first.created_at());
            assert_eq!(restarted.operator().revision_id(), first.operator().revision_id());
            services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped"); supplier.stop().await;
        })).await.expect("bounded column lifecycle");
    }));
}

#[test]
fn concurrent_creations_append_once_and_competing_names_have_one_winner() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(
            Duration::from_secs(40),
            Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    start(database.connections(&database.user), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let barrier = Arc::new(tokio::sync::Barrier::new(3));
                let mut tasks = Vec::new();
                for key in ["one", "two"] {
                    let service = services.column().clone();
                    let actor = actor.clone();
                    let barrier = Arc::clone(&barrier);
                    tasks.push(tokio::spawn(async move {
                        barrier.wait().await;
                        service
                            .create(actor, operation(), input(key, "Shared", true))
                            .await
                    }));
                }
                barrier.wait().await;
                let mut successes = 0;
                let mut conflicts = 0;
                for task in tasks {
                    match task.await.expect("concurrent creator") {
                        Ok(created) => {
                            assert_eq!(created.column().index, 0);
                            successes += 1;
                        }
                        Err(Error::Rejected(Rejection::NameInUse)) => conflicts += 1,
                        other => panic!("unexpected outcome: {:?}", other.err()),
                    }
                }
                assert_eq!((successes, conflicts), (1, 1));
                let appended = services
                    .column()
                    .create(actor, operation(), input("next", "Next", true))
                    .await
                    .expect("append after competing creations");
                assert_eq!(appended.column().index, 1);
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                supplier.stop().await;
            }),
        )
        .await
        .expect("bounded concurrent creation");
    }));
}

#[test]
fn unknown_commit_recovers_the_same_creation_after_commit_or_rollback() {
    use fixture::database_proxy::{Fault, Proxy};
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for fault in [Fault::LostCommit, Fault::RolledBack] {
            timeout(
                Duration::from_secs(40),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) =
                        start(authentication::proxied(&database, &proxy), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let original = operation();
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    proxy.arm(fault);
                    let service = services.column().clone();
                    let writer = actor.clone();
                    let waiter = tokio::spawn(async move {
                        service
                            .create(writer, original, input("news", "News", true))
                            .await
                    });
                    timeout(Duration::from_secs(2), proxy.intercepted())
                        .await
                        .expect("real COMMIT intercepted");
                    proxy.release();
                    let result = waiter.await.expect("writer task");
                    if rolled_back {
                        assert!(matches!(result, Err(Error::Unavailable(_))));
                        let observer = database.connections(&database.user).recover().await.expect("rollback observer");
                        let counts = observer.client().query_one("SELECT (SELECT count(*) FROM content.columns), (SELECT count(*) FROM content.changes), (SELECT count(*) FROM content.appends), (SELECT count(*) FROM content.completions)", &[]).await.expect("confirmed rollback");
                        for index in 0..4 { assert_eq!(counts.get::<_, i64>(index), 0); }
                        observer.terminate().await.expect("observer ended");
                    }
                    let replay = services
                        .column()
                        .create(actor.clone(), original, input("news", "News", true))
                        .await
                        .expect("original replay");
                    if !rolled_back {
                        let result = result.expect("committed original success");
                        assert_eq!(result.id(), replay.id());
                        assert_eq!(result.column(), replay.column());
                    }
                    assert_eq!(replay.column().index, 0);
                    let appended = services.column().create(actor, operation(), input("next", "Next", false)).await.expect("append after recovery");
                    assert_eq!(appended.column().index, 1);
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    proxy.stop().await;
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded commit recovery");
        }
    }));
}

#[test]
fn article_permission_alone_allows_creation_and_loss_rejects_new_replays() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(40), Box::pin(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(database.connections(&database.user), &supplier).await;
            let administrator = actor(&database, &services, &mut supplier).await;
            let member = register(&database, &services, &mut supplier).await;
            let role = services.role().create(administrator.clone(), operation(), serde_json::from_value(json!({"name":"Editor","level":2,"permissions":["institution.articles"]})).expect("role input")).await.expect("article-only role");
            let candidate = services.appointment().candidate(&administrator, "13800000000".parse().expect("member mobile")).await.expect("appointment candidate");
            services.appointment().assign(administrator.clone(), operation(), serde_json::from_value(json!({"target":{"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000","assignmentId":candidate.assignment_id()},"role":{"id":role.id(),"changeId":role.change_id()}})).expect("appointment input")).await.expect("assign editor");
            let original = operation();
            let created = services.column().create(member.clone(), original, input("news", "News", true)).await.expect("low-level article-only editor");
            assert_eq!(created.column().index, 0);
            services.role().update(administrator.clone(), operation(), role.id(), serde_json::from_value(json!({"changeId":role.change_id(),"name":"Editor","level":2,"permissions":[]})).expect("loss input")).await.expect("remove article permission");
            assert!(matches!(services.column().create(member.clone(), original, input("news", "News", true)).await, Err(Error::Denied)));
            assert!(matches!(services.column().create(member, operation(), input("next", "Next", true)).await, Err(Error::Denied)));
            let replay = services.column().create(administrator, original, input("news", "News", true)).await.expect("qualified original read");
            assert_eq!(replay.id(), created.id()); assert_eq!(replay.operator().revision_id(), created.operator().revision_id());
            services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        })).await.expect("bounded authority lifecycle");
    }));
}

#[test]
fn collection_limit_preserves_all_existing_positions_and_original_completions() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(
            Duration::from_secs(120),
            Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    start(database.connections(&database.user), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let first_operation = operation();
                let mut created = Vec::new();
                for index in 0..100 {
                    let operation = if index == 0 {
                        first_operation
                    } else {
                        operation()
                    };
                    let result = services
                        .column()
                        .create(
                            actor.clone(),
                            operation,
                            input(
                                &format!("column-{index}"),
                                &format!("Column {index}"),
                                index % 2 == 0,
                            ),
                        )
                        .await
                        .expect("bounded creation");
                    assert_eq!(result.column().index, index);
                    created.push(result);
                }
                assert!(matches!(
                    services
                        .column()
                        .create(
                            actor.clone(),
                            operation(),
                            input("overflow", "Overflow", true)
                        )
                        .await,
                    Err(Error::Rejected(Rejection::Limit))
                ));
                let replay = services
                    .column()
                    .create(
                        actor.clone(),
                        first_operation,
                        input("column-0", "Column 0", true),
                    )
                    .await
                    .expect("original succeeds at capacity");
                assert_eq!(replay.column(), created[0].column());
                let observer = database.connections(&database.user).recover().await.expect("capacity observer");
                let counts = observer.client().query_one("SELECT (SELECT count(*) FROM content.columns), (SELECT count(*) FROM content.changes), (SELECT count(*) FROM content.appends), (SELECT count(*) FROM content.completions)", &[]).await.expect("no partial overflow writes");
                for index in 0..4 { assert_eq!(counts.get::<_, i64>(index), 100); }
                observer.terminate().await.expect("observer ended");
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                supplier.stop().await;
            }),
        )
        .await
        .expect("bounded capacity acceptance");
    }));
}

#[test]
fn unpaired_writes_are_rejected_and_retained_damage_blocks_creation_and_startup() {
    use crate::{
        database::Barrier,
        runtime::Context,
        startup::{Services, Settings},
    };
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for table in ["columns", "navigations", "changes", "appends", "completions"] {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let original = operation();
                services.column().create(actor.clone(), original, input("news", "News", true)).await.expect("original creation");
                let runtime = database.connections(&database.user).recover().await.expect("runtime observer");
                let error = runtime.client().batch_execute("BEGIN; INSERT INTO content.completions SELECT family,gen_random_uuid(),action,target,input,subject,person_id,revision_id,created_at,expires_at,seal,seal_key_id,seal_version FROM content.completions; COMMIT").await.expect_err("a completion cannot commit without a creation");
                assert_eq!(error.code(),Some(&tokio_postgres::error::SqlState::FOREIGN_KEY_VIOLATION));
                runtime.client().batch_execute("ROLLBACK").await.expect("failed transaction cleared");
                runtime.terminate().await.expect("runtime ended");
                let owner = database.connections("postgres").recover().await.expect("isolated damage owner");
                owner.client().batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; UPDATE content.{table} SET seal=set_byte(seal,0,get_byte(seal,0) # 1); COMMIT")).await.expect("selected sealed fact damage");
                let result = if matches!(table, "changes" | "appends" | "completions") {
                    services.column().create(actor.clone(), original, input("news", "News", true)).await.map(|_| ())
                } else {
                    services.column().create(actor.clone(), operation(), input("next", "Next", true)).await.map(|_| ())
                };
                assert!(result.expect_err("damaged facts rejected").is_corrupt(),"{table}");
                assert!(services.ready().await.is_err());
                owner.terminate().await.expect("owner ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let context = Context::new(database.connections(&database.user)).await.expect("fresh context");
                let restarted = Box::pin(Services::start(
                    Barrier::begin(context.clone()).await.expect("restart barrier"),
                    authentication::inventory(authentication::keys()),
                    Settings::new(4, crate::identity::message::Limits::default(), authentication::session_settings(), crate::registration::Limits::default(), authentication::avatar_settings()).expect("settings"),
                    supplier.client(), authentication::unused_client().await,
                )).await;
                assert!(matches!(restarted, Err(crate::startup::Error::Column(_))),"damaged {table} blocks startup");
                context.stop().await.expect("restart stopped"); supplier.stop().await;
            })).await.expect("bounded structural and tamper checks");
        }
    }));
}
