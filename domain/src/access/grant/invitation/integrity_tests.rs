#![allow(clippy::expect_used)]

use std::{sync::Arc, time::Duration};

use fixture::database_proxy::{Fault, Proxy};
use serde_json::json;
use tokio::{runtime::Runtime, time::timeout};

use super::tests::{body, creator, input, later, operation, text, workforce_role};
use crate::{
    access::{
        grant::Error,
        role::{Kind, tests::start},
    },
    identity::authentication::{self, Database, Supplier, server},
};

#[test]
fn each_next_writer_resolves_a_committed_but_unsettled_predecessor() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for case in ["adjust", "withdraw", "accept"] {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let selected = workforce_role(&services, &manager, Kind::Default).await;
                let expiry = later();
                let created = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 2, expiry)))).await.expect("invitation");
                let writer = database.connections(&database.user).recover().await.expect("fixture writer");
                writer.client().batch_execute("BEGIN").await.expect("fixture transaction");
                crate::kernel::tx::object(writer.client(), b"access/invitation", created.id().as_bytes()).await.expect("object lock");
                let original = super::tests::state(writer.client(), &services, created.id()).await;
                let operation_id = operation();
                let input: super::adjustment::Input = serde_json::from_value(json!({"expected":{"capacity":2,"expiresAt":expiry},"capacity":3,"expiresAt":expiry})).expect("adjustment input");
                let change = super::transition::adjust(&original, super::transition::Adjustment {
                    id: serde_json::from_value(json!(uuid::Uuid::new_v4())).expect("adjustment ID"),
                    capacity: input.capacity, expires_at: input.expires_at,
                }, original.operator_id(), operation_id, original.changed_at()).expect("same-time adjustment");
                let pending = change.write(writer.client(), services.app().keys(), services.app().anchors()).await.expect("pending state");
                let authority = crate::kernel::Authorization { person_id: original.person_id(), revision_id: original.operator_id(), now: original.changed_at() };
                let request = crate::kernel::completion::Request::new::<std::convert::Infallible>("adjust", uuid::Uuid::from_bytes(*created.id().as_bytes()).to_string(), &input, &authority).expect("completed input");
                crate::kernel::completion::insert::<std::convert::Infallible>(writer.client(), services.app().keys(), super::adjustment::COMPLETIONS, operation_id, &request, original.changed_at(), None).await.expect("completion");
                writer.client().batch_execute("COMMIT").await.expect("commit releases the object lock before settling the anchor");
                match case {
                    "adjust" => { Box::pin(services.grant().adjust_invitation(manager, operation(), created.id(), serde_json::from_value(json!({"expected":{"capacity":3,"expiresAt":expiry},"capacity":4,"expiresAt":expiry})).expect("next adjustment"))).await.expect("adjust after pending commit"); }
                    "withdraw" => { Box::pin(services.grant().withdraw_invitation(manager, operation(), created.id())).await.expect("withdraw after pending commit"); }
                    _ => { Box::pin(services.grant().accept_invitation(holder, operation(), serde_json::from_value(json!({"code":text(&created),"changeId":selected.change_id()})).expect("acceptance"))).await.expect("accept after pending commit"); }
                }
                pending.committed().expect("late original settlement remains idempotent");
                let row = writer.client().query_one("SELECT sequence,version,used FROM access.invitations WHERE id=$1", &[&created.id()]).await.expect("both transitions committed");
                assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1),row.get::<_,i64>(2)), (3,3,i64::from(case=="accept")));
                writer.terminate().await.expect("writer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.close(); services.drain().await.expect("restart verifies both completions"); context.stop().await.expect("stopped"); supplier.stop().await;
            })).await.expect("bounded pending predecessor");
        }
    }));
}

#[test]
fn startup_requires_continuous_records_and_the_exact_completed_input() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for case in ["record gap", "completed input", "missing completion", "orphan completion"] {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (_, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let selected = workforce_role(&services, &manager, Kind::Default).await;
                let expiry = later();
                let created = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 1, expiry)))).await.expect("invitation");
                let operation_id = operation();
                let adjusted = Box::pin(services.grant().adjust_invitation(manager.clone(), operation_id, created.id(), serde_json::from_value(json!({"expected":{"capacity":1,"expiresAt":expiry},"capacity":2,"expiresAt":expiry})).expect("adjustment"))).await.expect("adjusted");
                let actor = services.current().read(&manager).await.expect("creator");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                match case {
                    "record gap" => { owner.client().batch_execute("DELETE FROM access.adjustments").await.expect("remove head record"); }
                    "orphan completion" => { owner.client().batch_execute("DELETE FROM access.adjustments; DELETE FROM access.creations; DELETE FROM access.invitations").await.expect("remove object but retain completions"); }
                    _ => {
                        owner.client().execute("DELETE FROM access.completions WHERE family='invitation_adjustment' AND operation_id=$1", &[&operation_id]).await.expect("remove completion");
                        if case == "completed input" {
                            let authority = crate::kernel::Authorization { person_id: actor.person_id(), revision_id: actor.revision_id(), now: adjusted.adjusted_at() };
                            let input: super::adjustment::Input = serde_json::from_value(json!({"expected":{"capacity":99,"expiresAt":expiry},"capacity":2,"expiresAt":expiry})).expect("well-formed but wrong expected terms");
                            let request = crate::kernel::completion::Request::new::<std::convert::Infallible>("adjust", uuid::Uuid::from_bytes(*created.id().as_bytes()).to_string(), &input, &authority).expect("canonical completion input");
                            crate::kernel::completion::insert::<std::convert::Infallible>(owner.client(), services.app().keys(), super::adjustment::COMPLETIONS, operation_id, &request, adjusted.adjusted_at(), None).await.expect("authentically sealed wrong input");
                        }
                    }
                }
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result = Box::pin(crate::startup::Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("barrier"),
                    authentication::inventory(authentication::keys()),
                    crate::startup::Settings::new(4, crate::identity::message::Limits::default(), authentication::session_settings(), crate::registration::Limits::default(), authentication::avatar_settings()).expect("settings"),
                    supplier.client(), authentication::unused_client().await,
                )).await;
                assert!(matches!(result, Err(crate::startup::Error::Grant(Error::Command(crate::kernel::Error::Corrupt(_))))), "{case}: {:?}", result.as_ref().err());
                context.stop().await.expect("failed startup stopped"); owner.terminate().await.expect("owner ended"); supplier.stop().await;
            })).await.expect("bounded startup integrity");
        }
    }));
}

#[test]
fn completion_expiry_rolls_back_every_state_record_and_completion() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for case in ["adjust", "withdraw", "accept"] {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let selected = workforce_role(&services, &manager, Kind::Default).await;
                let expiry = later();
                let created = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 1, expiry)))).await.expect("invitation");
                let attempt = operation();
                proxy.arm(Fault::ExpiredInvitationCompletion);
                let result = match case {
                    "adjust" => Box::pin(services.grant().adjust_invitation(manager, attempt, created.id(), serde_json::from_value(json!({
                        "expected":{"capacity":1,"expiresAt":expiry}, "capacity":2,
                        "expiresAt":expiry + chrono::Duration::days(3)
                    })).expect("extension beyond completion observation"))).await.map(|_| ()),
                    "withdraw" => Box::pin(services.grant().withdraw_invitation(manager, attempt, created.id())).await.map(|_| ()),
                    _ => Box::pin(services.grant().accept_invitation(holder, attempt, serde_json::from_value(json!({"code":text(&created),"changeId":selected.change_id()})).expect("acceptance"))).await.map(|_| ()),
                };
                if case == "accept" { assert!(matches!(result, Err(Error::InvitationUnavailable)), "{result:?}"); }
                else { assert!(matches!(result, Err(Error::StateConflict)), "{result:?}"); }
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let row = observer.client().query_one("SELECT current_capacity, current_expiry, used, withdrawn_at, sequence, version FROM access.invitations WHERE id=$1", &[&created.id()]).await.expect("original state");
                assert_eq!((row.get::<_,i64>(0), row.get::<_,chrono::DateTime<chrono::Utc>>(1), row.get::<_,i64>(2), row.get::<_,Option<chrono::DateTime<chrono::Utc>>>(3), row.get::<_,i64>(4), row.get::<_,i64>(5)), (1, expiry, 0, None, 1, 1));
                let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.invitation_transitions WHERE operation_id=$1), (SELECT count(*) FROM access.grants WHERE operation_id=$1), (SELECT count(*) FROM access.completions WHERE operation_id=$1)", &[&attempt]).await.expect("whole transaction rolled back");
                assert_eq!((row.get::<_,i64>(0), row.get::<_,i64>(1), row.get::<_,i64>(2)), (0,0,0));
                let restored = super::tests::state(observer.client(), &services, created.id()).await;
                assert_eq!(restored.used(), 0, "rollback restores the process anchor too");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                supplier.stop().await;
            })).await.expect("bounded completion expiry");
        }
    }));
}

#[test]
fn online_absence_locator_changes_and_rollback_are_corruption() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        for case in ["deleted", "lookup moved", "creator moved", "rollback"] {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let selected = workforce_role(&services, &manager, Kind::Default).await;
                let expiry = later();
                let created = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 1, expiry)))).await.expect("invitation");
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                owner.client().batch_execute("CREATE TEMP TABLE original AS SELECT * FROM access.invitations").await.expect("exact original state");
                Box::pin(services.grant().adjust_invitation(manager.clone(), operation(), created.id(), serde_json::from_value(json!({"expected":{"capacity":1,"expiresAt":expiry},"capacity":2,"expiresAt":expiry})).expect("adjustment"))).await.expect("advance anchored state");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                let sql = match case {
                    "deleted" => "DELETE FROM access.invitations",
                    "lookup moved" => "UPDATE access.invitations SET hash=set_byte(hash,0,get_byte(hash,0)#1)",
                    "creator moved" => "UPDATE access.invitations SET person_id=gen_random_uuid()",
                    _ => "UPDATE access.invitations state SET current_capacity=old.current_capacity, current_expiry=old.current_expiry, changed_at=old.changed_at, sequence=old.sequence, version=old.version, seal=old.seal FROM original old WHERE state.id=old.id",
                };
                owner.client().batch_execute(sql).await.expect("damage");
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                let result = if case == "lookup moved" {
                    Box::pin(services.grant().invitation_candidate(&holder, serde_json::from_value(json!({"code":text(&created)})).expect("code"))).await.map(|_| ())
                } else {
                    services.grant().read_invitation(&manager, created.id()).await.map(|_| ())
                };
                let error = result.expect_err("corruption is not ordinary absence");
                assert!(crate::runtime::work::Failure::unreliable(&error), "{case}: {error:?}");
                assert!(services.ready().await.is_err(), "corruption closes shared admission");
                owner.terminate().await.expect("owner ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            })).await.expect("bounded online integrity");
        }
    }));
}
