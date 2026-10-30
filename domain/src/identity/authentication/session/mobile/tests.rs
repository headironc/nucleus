use std::{sync::Arc, time::Duration};

use fixture::database_proxy::{Fault, Proxy};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use super::Input;
use crate::{
    database::Barrier,
    identity::{
        authentication::{
            self, Database, Supplier, avatar_settings, inventory, keys, proxied, server,
            session::{Receipt, tests::account},
        },
        current::Proof,
        session,
    },
    runtime::Context,
    startup::{Services, Settings},
};

const SECRET: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

#[test]
fn startup_exchange_row_timeout_releases_the_scan_and_allows_restart() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(40), async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(&supplier, database.connections(&database.user)).await;
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move {
                service
                    .create_mobile_session(input(Uuid::new_v4(), "retained-row-code"))
                    .await
            });
            reply(&mut supplier, &mut worker, "13800000000", true).await;
            worker.await.expect("worker").expect("retained exchange");
            services.drain().await.expect("drained");
            context.stop().await.expect("stopped");
            drop(services);

            let proxy = Proxy::start(&server).await;
            let context = Context::new(proxied(&database, &proxy))
                .await
                .expect("startup runtime");
            let barrier = Barrier::begin(context.clone())
                .await
                .expect("startup barrier");
            proxy.arm(Fault::StalledExchangeRead);
            let settings = Settings::new(
                8,
                crate::identity::message::Limits::default(),
                authentication::session_settings()
                    .with_mini("mini-fixture".parse().expect("mini identity"), 2_592_000)
                    .expect("separate identities"),
                crate::registration::Limits::default(),
                avatar_settings(),
            )
            .expect("work capacity");
            let startup = Box::pin(Services::start_with_mini(
                barrier,
                inventory(keys()),
                settings,
                supplier.client(),
                authentication::dispatch::tests::unused_client().await,
                Some(supplier.mini_client()),
            ));
            let (result, _) = timeout(Duration::from_secs(15), async {
                tokio::join!(startup, async {
                    timeout(Duration::from_secs(3), proxy.intercepted())
                        .await
                        .expect("the retained row lookup was intercepted")
                })
            })
            .await
            .expect("the existing batch budget also bounds each row lookup");
            assert!(matches!(result, Err(crate::startup::Error::Authentication(
                authentication::Error::VerificationTimeout(cause)
            )) if cause.stage() == crate::database::verification::Stage::ExchangesBatch
                && cause.waiting() == crate::database::verification::Waiting::QueryAndValidation));
            proxy.release();
            context.stop().await.expect("failed startup runtime ended");
            proxy.stop().await;

            let observer = database
                .connections(&database.user)
                .recover()
                .await
                .expect("observer");
            assert!(
                observer
                    .client()
                    .query_one("SELECT pg_try_advisory_xact_lock(1314210636, 1)", &[])
                    .await
                    .expect("independent barrier observation")
                    .get::<_, bool>(0)
            );
            observer.terminate().await.expect("observer ended");
            let (context, services) = start(&supplier, database.connections(&database.user)).await;
            services.drain().await.expect("restarted services drained");
            context.stop().await.expect("restarted runtime ended");
            assert!(
                timeout(Duration::from_millis(20), supplier.observe())
                    .await
                    .is_err(),
                "startup never repeats a phone exchange"
            );
            supplier.stop().await;
        })
        .await
        .expect("retained exchange timeout and restart budget");
    });
}

#[test]
fn administration_requires_the_retained_mini_identity_without_loading_supplier_credentials() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(25), async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(&supplier, database.connections(&database.user)).await;
            let service = services.authentication().clone();
            let operation = Uuid::new_v4();
            let mut worker = tokio::spawn(async move {
                service
                    .create_mobile_session(input(operation, "retained-identity-code"))
                    .await
            });
            reply(&mut supplier, &mut worker, "13800000000", true).await;
            assert!(matches!(
                worker.await.expect("worker").expect("authentication"),
                Receipt::Registration(_)
            ));
            services.drain().await.expect("drained");
            context.stop().await.expect("stopped");
            drop(services);
            for mini in [None, Some("different-mini"), Some("mini-fixture")] {
                let context = Context::new(database.connections(&database.user))
                    .await
                    .expect("maintenance runtime");
                let settings = session::settings::Settings::new(
                    "website-fixture".parse().expect("website identity"),
                    86_400,
                    600,
                )
                .expect("website settings");
                let settings = match mini {
                    Some(app) => settings
                        .with_mini(app.parse().expect("mini identity"), 2_592_000)
                        .expect("mini settings"),
                    None => settings,
                };
                let result = crate::startup::Administration::start_with_sessions(
                    Barrier::begin(context.clone())
                        .await
                        .expect("maintenance barrier"),
                    inventory(keys()),
                    settings,
                )
                .await;
                if mini == Some("mini-fixture") {
                    result
                        .expect("configured source passes all startup scanners")
                        .drain()
                        .await
                        .expect("maintenance drained");
                } else {
                    assert!(
                        result.is_err(),
                        "missing or changed retained identity must be refused"
                    );
                }
                context.stop().await.expect("maintenance stopped");
            }
            assert!(
                timeout(Duration::from_millis(20), supplier.observe())
                    .await
                    .is_err(),
                "maintenance never calls suppliers"
            );
            supplier.stop().await;
        })
        .await
        .expect("retained-source startup budget");
    });
}

fn input(operation: Uuid, code: &str) -> Input {
    Input::new(
        operation.to_string().parse().expect("operation"),
        SECRET.parse().expect("secret"),
        code.parse().expect("code"),
    )
}

async fn start(
    supplier: &Supplier,
    connections: platform::database::Connections,
) -> (Context, Services) {
    let context = Context::new(connections).await.expect("isolated runtime");
    let settings = Settings::new(
        8,
        crate::identity::message::Limits::default(),
        authentication::session_settings()
            .with_mini("mini-fixture".parse().expect("mini identity"), 2_592_000)
            .expect("separate identities"),
        crate::registration::Limits::default(),
        avatar_settings(),
    )
    .expect("work capacity");
    let services = Services::start_with_mini(
        Barrier::begin(context.clone())
            .await
            .expect("startup barrier"),
        inventory(keys()),
        settings,
        supplier.client(),
        authentication::dispatch::tests::unused_client().await,
        Some(supplier.mini_client()),
    )
    .await
    .expect("verified mini service");
    (context, services)
}

async fn request(
    supplier: &mut Supplier,
    worker: &mut tokio::task::JoinHandle<Result<Receipt, authentication::Error>>,
) -> authentication::verification::tests::Request {
    tokio::select! {
        request = supplier.request() => request,
        result = worker => match result {
            Ok(Err(cause)) => panic!("operation ended before the expected supplier request: {cause:?}"),
            _ => panic!("operation ended before its expected supplier response"),
        }
    }
}

async fn reply(
    supplier: &mut Supplier,
    worker: &mut tokio::task::JoinHandle<Result<Receipt, authentication::Error>>,
    mobile: &str,
    token: bool,
) {
    if token {
        let request = request(supplier, worker).await;
        assert_eq!(request.body()["grant_type"], "client_credential");
        assert_eq!(request.body()["appid"], "mini-fixture");
        assert_eq!(request.body()["force_refresh"], false);
        request.reply(
            200,
            r#"{"access_token":"fictional-access-token","expires_in":7200}"#.into(),
        );
    }
    let request = request(supplier, worker).await;
    assert!(request.body()["code"].is_string());
    request.reply(200, serde_json::json!({"errcode":0,"phone_info":{"phoneNumber":format!("+86{mobile}"),"purePhoneNumber":mobile,"countryCode":"86","watermark":{"appid":"mini-fixture"}}}).to_string());
}

#[test]
fn mini_phone_registration_keeps_source_and_original_session_after_consumption() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(35), async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(&supplier, database.connections(&database.user)).await;
            let operation = Uuid::new_v4();
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move {
                service
                    .create_mobile_session(input(operation, "fictional-code"))
                    .await
            });
            reply(&mut supplier, &mut worker, "13800000000", true).await;
            let Receipt::Registration(credential) =
                worker.await.expect("worker").expect("registrable phone")
            else {
                panic!("pending credential only");
            };
            let r2 = Uuid::new_v4();
            let registration = || {
                crate::registration::Input::new(
                    r2.to_string().parse().expect("operation"),
                    credential.id(),
                    credential
                        .token()
                        .text()
                        .parse()
                        .expect("registration token"),
                    "隔离测试".parse().expect("name"),
                    "00000020000229007X".parse().expect("number"),
                )
            };
            let prepared = services
                .registration()
                .prepare(registration())
                .await
                .expect("verified source");
            assert!(matches!(prepared.origin(), session::Origin::MiniMobile(_)));
            let registered = services
                .registration()
                .register(prepared)
                .await
                .expect("atomic registration");
            assert!(registered.created());
            let actor = services
                .current()
                .authenticate(Proof::Bearer {
                    token: registered.token().text().parse().expect("bearer"),
                })
                .await
                .expect("current mini actor");
            let profile = services
                .current()
                .read(&actor)
                .await
                .expect("complete profile");
            assert_eq!(profile.name().as_ref(), "隔离测试");
            assert!(matches!(actor.origin(), session::Origin::MiniMobile(_)));
            assert!(matches!(
                services
                    .current()
                    .authenticate(Proof::Cookie {
                        id: registered.id(),
                        token: registered.token().text().parse().expect("same token")
                    })
                    .await,
                Err(crate::identity::current::Error::Invalid)
            ));
            assert!(
                services.admitted(),
                "valid mini token presented as cookie is an ordinary refusal"
            );
            let prepared = services
                .registration()
                .prepare(registration())
                .await
                .expect("receipt preserves source after credential consumption");
            let replay = services
                .registration()
                .register(prepared)
                .await
                .expect("original R2");
            assert!(!replay.created());
            assert_eq!(replay.id(), registered.id());
            assert_eq!(
                replay.token().text().as_str(),
                registered.token().text().as_str()
            );
            assert_eq!(replay.expires_at(), registered.expires_at());
            assert!(matches!(
                services
                    .authentication()
                    .create_mobile_session(input(operation, "fictional-code"))
                    .await,
                Err(authentication::Error::Verification)
            ));
            services.drain().await.expect("work drained");
            context.stop().await.expect("runtime stopped");
            supplier.stop().await;
        })
        .await
        .expect("registration acceptance budget");
    });
}

#[test]
fn mini_phone_existing_and_suspended_accounts_preserve_their_qualification() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(50), async {
            for suspended in [false, true] {
                let database = Database::new(Arc::clone(&server)).await;
                account(&database, "13800000000", suspended).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    start(&supplier, database.connections(&database.user)).await;
                let operation = Uuid::new_v4();
                let service = services.authentication().clone();
                let mut worker = tokio::spawn(async move {
                    service
                        .create_mobile_session(input(operation, "phone-code"))
                        .await
                });
                reply(&mut supplier, &mut worker, "13800000000", true).await;
                let outcome = worker.await.expect("worker");
                if suspended {
                    assert!(matches!(outcome, Err(authentication::Error::Suspended)));
                } else {
                    let Receipt::Session(issued) = outcome.expect("session") else {
                        panic!("existing account session");
                    };
                    let Receipt::Session(replayed) = services
                        .authentication()
                        .create_mobile_session(input(operation, "phone-code"))
                        .await
                        .expect("original session")
                    else {
                        panic!("original session");
                    };
                    assert_eq!(issued.id(), replayed.id());
                    assert_eq!(
                        issued.token().text().as_str(),
                        replayed.token().text().as_str()
                    );
                    let actor = services
                        .current()
                        .authenticate(Proof::Bearer {
                            token: issued.token().text().parse().expect("token"),
                        })
                        .await
                        .expect("actor");
                    services.current().read(&actor).await.expect("self profile");
                }
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                supplier.stop().await;
            }
        })
        .await
        .expect("account routes budget");
    });
}

#[test]
fn pending_phone_claim_rejects_other_proof_input_and_branch_before_waiting() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(35), async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let proxy = Proxy::start(&server).await;
            let (context, services) = start(&supplier, proxied(&database, &proxy)).await;
            proxy.arm(Fault::PausedCommit);
            let operation = Uuid::new_v4();
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move { service.create_mobile_session(input(operation, "original-code")).await });
            tokio::select! {
                _ = proxy.intercepted() => {},
                result = &mut worker => match result { Ok(Err(cause)) => panic!("claim ended before its commit gate: {cause:?}"), _ => panic!("claim gate was not reached") },
            }
            let mut wrong = input(operation, "original-code"); wrong.secret = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".parse().expect("other secret");
            assert!(matches!(timeout(Duration::from_secs(1), services.authentication().create_mobile_session(wrong)).await.expect("proof refusal does not wait"), Err(authentication::Error::Proof)));
            assert!(matches!(timeout(Duration::from_secs(1), services.authentication().create_mobile_session(input(operation, "other-code"))).await.expect("input refusal does not wait"), Err(authentication::Error::Mismatch)));
            let sms = super::super::Input::new(operation.to_string().parse().expect("operation"), SECRET.parse().expect("secret"), Uuid::new_v4().to_string().parse().expect("message id"), "13800000000".parse().expect("mobile"), "123456".parse().expect("SMS code"));
            assert!(matches!(timeout(Duration::from_secs(1), services.authentication().create_session(sms)).await.expect("branch refusal does not wait"), Err(authentication::Error::Mismatch)));
            assert!(timeout(Duration::from_millis(50), supplier.observe()).await.is_err(), "no call before confirmed occupancy");
            proxy.release();
            reply(&mut supplier, &mut worker, "13800000000", true).await;
            worker.await.expect("worker").expect("original result");
            assert!(matches!(services.authentication().create_mobile_session(input(Uuid::new_v4(), "original-code")).await, Err(authentication::Error::Verification)));
            services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await; proxy.stop().await;
        }).await.expect("claim synchronization budget");
    });
}

#[test]
fn phone_refusals_and_technical_failures_have_different_terminal_results() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(35), async {
            let success = |number: &str, country: &str, full: &str, app: &str| serde_json::json!({"errcode":0,"phone_info":{"phoneNumber":full,"purePhoneNumber":number,"countryCode":country,"watermark":{"appid":app}}}).to_string();
            for (body, first, replay) in [
                (r#"{"errcode":40029}"#.into(), authentication::Failure::Proof, authentication::Failure::Proof),
                (success("2025550100", "1", "+12025550100", "mini-fixture"), authentication::Failure::MobileUnsupported, authentication::Failure::MobileUnsupported),
                (success("14200000000", "86", "8614200000000", "mini-fixture"), authentication::Failure::MobileUnsupported, authentication::Failure::MobileUnsupported),
                (success("13800000000", "86", "8613800000000", "other-app"), authentication::Failure::Unavailable, authentication::Failure::Verification),
                (success("13800000000", "86", "8613900000000", "mini-fixture"), authentication::Failure::Unavailable, authentication::Failure::Verification),
                (r#"{"errcode":0}"#.into(), authentication::Failure::Unavailable, authentication::Failure::Verification),
                (r#"{"errcode":45009,"errmsg":"private-provider-payload"}"#.into(), authentication::Failure::Unavailable, authentication::Failure::Verification),
            ] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(&supplier, database.connections(&database.user)).await;
                let operation = Uuid::new_v4();
                let service = services.authentication().clone();
                let mut worker = tokio::spawn(async move { service.create_mobile_session(input(operation, "private-phone-code")).await });
                request(&mut supplier, &mut worker).await.reply(200, r#"{"access_token":"private-access-token","expires_in":7200}"#.into());
                request(&mut supplier, &mut worker).await.reply(200, body);
                let error = worker.await.expect("worker").err().expect("refusal");
                assert_eq!(error.failure(), first);
                let diagnostic = format!("{error:?}");
                for secret in ["private-phone-code", "private-access-token", "private-provider-payload", "fictional-mini-secret"] { assert!(!diagnostic.contains(secret)); }
                let error = services.authentication().create_mobile_session(input(operation, "private-phone-code")).await.err().expect("retained refusal");
                assert_eq!(error.failure(), replay);
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let row = observer.client().query_one("SELECT (SELECT count(*) FROM identity.authentications WHERE operation_id=$1), status FROM identity.exchanges WHERE operation_id=$1", &[&operation.to_string().parse::<crate::identity::operation::Id>().expect("operation")]).await.expect("terminal facts");
                assert_eq!(row.get::<_, i64>(0), 0); assert_eq!(row.get::<_, &str>(1), "exhausted");
                observer.terminate().await.expect("observer ended");
                assert!(services.admitted(), "supplier failure does not close unrelated work");
                assert!(timeout(Duration::from_millis(20), supplier.observe()).await.is_err(), "replay does not call the supplier");
                services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            }
        }).await.expect("failure matrix budget");
    });
}

#[test]
fn committed_claim_and_result_response_loss_never_reexchange_or_reissue() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(40), async {
            for claim in [true, false] {
                let database = Database::new(Arc::clone(&server)).await;
                account(&database, "13800000000", false).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(&supplier, proxied(&database, &proxy)).await;
                let operation = Uuid::new_v4();
                if claim { proxy.arm(Fault::LostCommit); }
                let service = services.authentication().clone();
                let mut worker = tokio::spawn(async move { service.create_mobile_session(input(operation, "lost-response-code")).await });
                request(&mut supplier, &mut worker).await.reply(200, r#"{"access_token":"fixture-token","expires_in":7200}"#.into());
                let phone = request(&mut supplier, &mut worker).await;
                if claim { proxy.release(); } else { proxy.arm(Fault::LostCommit); }
                phone.reply(200, serde_json::json!({"errcode":0,"phone_info":{"phoneNumber":"8613800000000","purePhoneNumber":"13800000000","countryCode":"86","watermark":{"appid":"mini-fixture"}}}).to_string());
                let Receipt::Session(original) = worker.await.expect("worker").expect("readback success") else { panic!("existing account"); };
                proxy.release();
                let Receipt::Session(replay) = services.authentication().create_mobile_session(input(operation, "lost-response-code")).await.expect("same original") else { panic!("original session"); };
                assert_eq!(replay.id(), original.id()); assert_eq!(replay.token().text().as_str(), original.token().text().as_str()); assert_eq!(replay.expires_at(), original.expires_at());
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let row = observer.client().query_one("SELECT (SELECT count(*) FROM identity.exchanges), (SELECT count(*) FROM identity.sessions)", &[]).await.expect("atomic one-time result");
                assert_eq!(row.get::<_, i64>(0), 1); assert_eq!(row.get::<_, i64>(1), 1);
                observer.terminate().await.expect("observer ended");
                assert!(timeout(Duration::from_millis(20), supplier.observe()).await.is_err());
                services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await; proxy.stop().await;
            }
        }).await.expect("commit readback budget");
    });
}

#[test]
fn restart_closes_an_orphan_claim_without_obtaining_call_permission() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(20), async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(&supplier, database.connections(&database.user)).await;
            let operation = Uuid::new_v4();
            assert!(matches!(
                super::admit(
                    &services.authentication().0,
                    &input(operation, "orphan-code")
                )
                .await
                .expect("claim only"),
                super::Admission::Call(_)
            ));
            services.drain().await.expect("old work ended");
            context.stop().await.expect("old instance ended");
            drop(services);
            let (context, services) = start(&supplier, database.connections(&database.user)).await;
            assert!(matches!(
                services
                    .authentication()
                    .create_mobile_session(input(operation, "orphan-code"))
                    .await,
                Err(authentication::Error::Verification)
            ));
            assert!(matches!(
                services
                    .authentication()
                    .create_mobile_session(input(Uuid::new_v4(), "orphan-code"))
                    .await,
                Err(authentication::Error::Verification)
            ));
            assert!(
                timeout(Duration::from_millis(20), supplier.observe())
                    .await
                    .is_err()
            );
            let observer = database
                .connections(&database.user)
                .recover()
                .await
                .expect("observer");
            let row = observer
                .client()
                .query_one("SELECT status, reason FROM identity.exchanges", &[])
                .await
                .expect("closed original");
            assert_eq!(row.get::<_, &str>(0), "exhausted");
            assert_eq!(row.get::<_, &str>(1), "verification_required");
            observer.terminate().await.expect("observer ended");
            services.drain().await.expect("drained");
            context.stop().await.expect("stopped");
            supplier.stop().await;
        })
        .await
        .expect("restart budget");
    });
}

#[test]
fn runtime_cannot_reset_change_or_delete_a_claim_or_resolve_without_authentication() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(20), async {
            let database = Database::initialized(server).await;
            let supplier = Supplier::new().await;
            let (context, services) = start(&supplier, database.connections(&database.user)).await;
            let operation = Uuid::new_v4();
            super::admit(&services.authentication().0, &input(operation, "protected-code")).await.expect("claimed");
            let writer = database.connections(&database.user).recover().await.expect("runtime writer");
            for sql in [
                "INSERT INTO identity.exchanges (id,operation_id,app_id,proof,hash,key_id,code_hash,lookup_key_id,worker_id,claimed_at,deadline,status) SELECT gen_random_uuid(),gen_random_uuid(),app_id,proof,hash,key_id,decode(repeat('fe',32),'hex'),lookup_key_id,gen_random_uuid(),claimed_at,claimed_at+interval '31 seconds','claimed' FROM identity.exchanges",
                "UPDATE identity.exchanges SET status='claimed'",
                "UPDATE identity.exchanges SET status='exhausted',reason='verification_required',finished_at=clock_timestamp(),transaction_id=pg_current_xact_id(),worker_id=gen_random_uuid()",
                "DELETE FROM identity.exchanges",
                "BEGIN; UPDATE identity.exchanges SET status='resolved',mobile_hash=decode(repeat('01',32),'hex'),finished_at=clock_timestamp(),transaction_id=pg_current_xact_id(); COMMIT",
            ] {
                assert!(writer.client().batch_execute(sql).await.is_err(), "runtime rejects {sql}");
                writer.client().batch_execute("ROLLBACK").await.expect("failed attempt ended");
            }
            let row = writer.client().query_one("SELECT status, (SELECT count(*) FROM identity.authentications) FROM identity.exchanges", &[]).await.expect("original unchanged");
            assert_eq!(row.get::<_, &str>(0), "claimed"); assert_eq!(row.get::<_, i64>(1), 0);
            writer.terminate().await.expect("writer ended");
            services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        }).await.expect("SQL protection budget");
    });
}

#[test]
fn retained_code_key_blocks_a_new_operation_after_lookup_rotation() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(20), async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(&supplier, database.connections(&database.user)).await;
            super::admit(
                &services.authentication().0,
                &input(Uuid::new_v4(), "retained-code"),
            )
            .await
            .expect("old-key occupancy");
            let mut service = services.authentication().clone();
            drop(services);
            let roots = crate::keys::Keys::new(
                crate::keys::Id::new(1).expect("new active"),
                (0..2)
                    .map(|id| {
                        crate::keys::Entry::new(
                            crate::keys::Id::new(id).expect("root id"),
                            crate::keys::Material(zeroize::Zeroizing::new(
                                [u8::try_from(id + 1).expect("fixture byte"); 32],
                            )),
                        )
                    })
                    .collect(),
            )
            .expect("active and retained roots");
            // Isolate code-key rotation from unrelated lookup migrations, which have their own owner.
            let mut identity = keys();
            identity.challenge = roots.identity.challenge;
            Arc::get_mut(&mut service.0)
                .expect("only authentication owner")
                .keys = Arc::new(identity);
            assert!(matches!(
                service
                    .create_mobile_session(input(Uuid::new_v4(), "retained-code"))
                    .await,
                Err(authentication::Error::Verification)
            ));
            assert!(
                timeout(Duration::from_millis(20), supplier.observe())
                    .await
                    .is_err()
            );
            let observer = database
                .connections(&database.user)
                .recover()
                .await
                .expect("observer");
            assert_eq!(
                observer
                    .client()
                    .query_one("SELECT count(*) FROM identity.exchanges", &[])
                    .await
                    .expect("one occupancy")
                    .get::<_, i64>(0),
                1
            );
            observer.terminate().await.expect("observer ended");
            service.drain().await.expect("drained");
            context.stop().await.expect("stopped");
            supplier.stop().await;
        })
        .await
        .expect("retained-key occupancy budget");
    });
}

#[test]
fn phone_proof_encodings_match_independently_derived_hmac_vectors() {
    let request = input(Uuid::nil(), "test-code");
    let material = || {
        zeroize::Zeroizing::new(std::array::from_fn(|index| {
            u8::try_from(index).expect("32 fixture bytes")
        }))
    };
    let root = crate::keys::Id::new(0).expect("id");
    let input_keys = crate::identity::keys::request::Keys::new(crate::keys::Derived::fixed(
        root,
        vec![crate::keys::Entry::new(
            root,
            crate::keys::Material(material()),
        )],
    ));
    let code_keys = crate::identity::keys::challenge::Keys::new(crate::keys::Derived::fixed(
        root,
        vec![crate::keys::Entry::new(
            root,
            crate::keys::Material(material()),
        )],
    ));
    let app: crate::identity::mini::AppId = "mini-fixture".parse().expect("mini application");
    assert_eq!(
        hex::encode(
            input_keys
                .active()
                .sign(
                    &request
                        .encoded(&session::Origin::MiniMobile(app.clone()))
                        .expect("canonical command")
                )
                .as_ref()
        ),
        "4cc7dd8dcaa723b4581bb358ebbeb155ad425c94a838f33664fcbe8af6dcb81a"
    );
    assert_eq!(
        hex::encode(
            code_keys
                .active()
                .phone_code(&request.occupancy(&app).expect("code domain"))
                .as_ref()
        ),
        "ce4e07fcd3c935e307f711992ff823596e39b33edd5397860f303383fcbcc786"
    );
}

#[test]
fn rolled_back_and_unknown_claims_do_not_gain_another_exchange_attempt() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(35), async {
            for unknown in [false, true] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(&supplier, proxied(&database, &proxy)).await;
                proxy.arm(if unknown {
                    Fault::UnknownCommit
                } else {
                    Fault::RolledBack
                });
                let operation = Uuid::new_v4();
                let error = services
                    .authentication()
                    .create_mobile_session(input(operation, "unconfirmed-claim-code"))
                    .await
                    .err()
                    .expect("claim is not deliverable");
                assert_eq!(
                    error.failure(),
                    if unknown {
                        authentication::Failure::Unknown
                    } else {
                        authentication::Failure::Unavailable
                    }
                );
                assert!(
                    timeout(Duration::from_millis(20), supplier.observe())
                        .await
                        .is_err(),
                    "unconfirmed occupancy never calls supplier"
                );
                proxy.release();
                if unknown {
                    assert!(matches!(
                        services
                            .authentication()
                            .create_mobile_session(input(operation, "unconfirmed-claim-code"))
                            .await,
                        Err(authentication::Error::Verification)
                    ));
                    assert!(
                        timeout(Duration::from_millis(20), supplier.observe())
                            .await
                            .is_err(),
                        "recovery cannot call supplier"
                    );
                }
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                supplier.stop().await;
                proxy.stop().await;
            }
        })
        .await
        .expect("claim completion budget");
    });
}

#[test]
fn cancelled_waiter_keeps_the_original_exchange_supervised() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(20), async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(&supplier, database.connections(&database.user)).await;
            let operation = Uuid::new_v4();
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move { service.create_mobile_session(input(operation, "cancelled-waiter-code")).await });
            request(&mut supplier, &mut worker).await.reply(200, r#"{"access_token":"fixture-token","expires_in":7200}"#.into());
            let phone = request(&mut supplier, &mut worker).await;
            worker.abort(); match worker.await { Err(error) => assert!(error.is_cancelled()), Ok(_) => panic!("caller should be cancelled") }
            phone.reply(200, serde_json::json!({"errcode":0,"phone_info":{"phoneNumber":"8613800000000","purePhoneNumber":"13800000000","countryCode":"86","watermark":{"appid":"mini-fixture"}}}).to_string());
            assert!(matches!(services.authentication().create_mobile_session(input(operation, "cancelled-waiter-code")).await.expect("wait for supervised original"), Receipt::Registration(_)));
            assert!(timeout(Duration::from_millis(20), supplier.observe()).await.is_err());
            services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        }).await.expect("waiter cancellation budget");
    });
}

#[test]
fn prepared_registration_rechecks_replacement_and_a_later_authentication_uses_current_account() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(25), async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, services) = start(&supplier, database.connections(&database.user)).await;
            let operation = Uuid::new_v4();
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move {
                service
                    .create_mobile_session(input(operation, "old-code"))
                    .await
            });
            reply(&mut supplier, &mut worker, "13800000000", true).await;
            let Receipt::Registration(old) = worker.await.expect("worker").expect("old credential")
            else {
                panic!("no account yet");
            };
            let register = |credential: &super::super::Registration| {
                crate::registration::Input::new(
                    Uuid::new_v4().to_string().parse().expect("operation"),
                    credential.id(),
                    credential.token().text().parse().expect("token"),
                    "隔离测试".parse().expect("name"),
                    "00000020000229007X".parse().expect("number"),
                )
            };
            let prepared = services
                .registration()
                .prepare(register(&old))
                .await
                .expect("initial proof snapshot");
            let second = Uuid::new_v4();
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move {
                service
                    .create_mobile_session(input(second, "replacement-code"))
                    .await
            });
            reply(&mut supplier, &mut worker, "13800000000", false).await;
            let Receipt::Registration(new) = worker
                .await
                .expect("worker")
                .expect("replacement credential")
            else {
                panic!("no account yet");
            };
            assert_ne!(old.id(), new.id());
            assert!(matches!(
                services.registration().register(prepared).await,
                Err(crate::registration::Error::Verification)
            ));
            let registered = services
                .registration()
                .register(
                    services
                        .registration()
                        .prepare(register(&new))
                        .await
                        .expect("new proof"),
                )
                .await
                .expect("new atomic registration");
            let third = Uuid::new_v4();
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move {
                service
                    .create_mobile_session(input(third, "after-registration-code"))
                    .await
            });
            reply(&mut supplier, &mut worker, "13800000000", false).await;
            let Receipt::Session(current) = worker.await.expect("worker").expect("fresh login")
            else {
                panic!("current account controls final routing");
            };
            assert_ne!(current.id(), registered.id());
            services.drain().await.expect("drained");
            context.stop().await.expect("stopped");
            supplier.stop().await;
        })
        .await
        .expect("registration replacement budget");
    });
}

#[test]
fn a_registration_committing_between_mobile_discovery_and_lock_routes_to_its_account() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(25), async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let proxy = Proxy::start(&server).await;
            let (context, services) = start(&supplier, proxied(&database, &proxy)).await;
            let first = Uuid::new_v4();
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move { service.create_mobile_session(input(first, "credential-code")).await });
            reply(&mut supplier, &mut worker, "13800000000", true).await;
            let Receipt::Registration(credential) = worker.await.expect("worker").expect("credential") else { panic!("no account initially"); };
            let register = crate::registration::Input::new(Uuid::new_v4().to_string().parse().expect("operation"), credential.id(), credential.token().text().parse().expect("token"), "隔离测试".parse().expect("name"), "00000020000229007X".parse().expect("number"));
            let prepared = services.registration().prepare(register).await.expect("registration ready");
            let second = Uuid::new_v4();
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move { service.create_mobile_session(input(second, "concurrent-code")).await });
            let phone = request(&mut supplier, &mut worker).await;
            proxy.arm(Fault::PausedMobileLookup);
            phone.reply(200, serde_json::json!({"errcode":0,"phone_info":{"phoneNumber":"8613800000000","purePhoneNumber":"13800000000","countryCode":"86","watermark":{"appid":"mini-fixture"}}}).to_string());
            proxy.intercepted().await;
            let registered = services.registration().register(prepared).await.expect("R2 commits after empty discovery");
            proxy.release();
            let Receipt::Session(issued) = worker.await.expect("worker").expect("rebuild locks using the same trusted phone") else { panic!("new current account must be used"); };
            assert_ne!(issued.id(), registered.id());
            let actor = services.current().authenticate(Proof::Bearer { token: issued.token().text().parse().expect("token") }).await.expect("new-account actor");
            assert_eq!(services.current().read(&actor).await.expect("profile").name().as_ref(), "隔离测试");
            assert!(timeout(Duration::from_millis(20), supplier.observe()).await.is_err(), "local lock retry never reexchanges code");
            services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await; proxy.stop().await;
        }).await.expect("registration race budget");
    });
}

#[test]
fn a_known_original_result_survives_read_only_commit_response_loss() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(25), async {
            let database = Database::new(Arc::clone(&server)).await;
            account(&database, "13800000000", false).await;
            let mut supplier = Supplier::new().await;
            let proxy = Proxy::start(&server).await;
            let (context, services) = start(&supplier, proxied(&database, &proxy)).await;
            let operation = Uuid::new_v4();
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move {
                service
                    .create_mobile_session(input(operation, "known-result-code"))
                    .await
            });
            reply(&mut supplier, &mut worker, "13800000000", true).await;
            let Receipt::Session(original) = worker.await.expect("worker").expect("original")
            else {
                panic!("session");
            };
            proxy.arm(Fault::LostReadCommit);
            let Receipt::Session(replay) = services
                .authentication()
                .create_mobile_session(input(operation, "known-result-code"))
                .await
                .expect("known read remains successful")
            else {
                panic!("original session");
            };
            proxy.release();
            assert_eq!(replay.id(), original.id());
            assert_eq!(
                replay.token().text().as_str(),
                original.token().text().as_str()
            );
            let refused = Uuid::new_v4();
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move {
                service
                    .create_mobile_session(input(refused, "invalid-code"))
                    .await
            });
            request(&mut supplier, &mut worker)
                .await
                .reply(200, r#"{"errcode":40029}"#.into());
            assert!(matches!(
                worker.await.expect("worker"),
                Err(authentication::Error::MobileCodeInvalid)
            ));
            proxy.arm(Fault::LostReadCommit);
            assert!(matches!(
                services
                    .authentication()
                    .create_mobile_session(input(refused, "invalid-code"))
                    .await,
                Err(authentication::Error::MobileCodeInvalid)
            ));
            proxy.release();
            services.drain().await.expect("drained");
            context.stop().await.expect("stopped");
            supplier.stop().await;
            proxy.stop().await;
        })
        .await
        .expect("known-read completion budget");
    });
}

#[test]
fn committed_session_revoked_by_website_before_handoff_readback_is_a_known_refusal() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(25), async {
            let database = Database::new(Arc::clone(&server)).await;
            account(&database, "13800000000", false).await;
            let mut supplier = Supplier::new().await;
            let proxy = Proxy::start(&server).await;
            let (context, services) = start(&supplier, proxied(&database, &proxy)).await;
            let message = authentication::session::tests::message(&database, services.authentication(), &mut supplier, "13800000000").await;
            let Receipt::Session(website) = services.authentication().create_session(authentication::session::tests::input(Uuid::new_v4(), message, "13800000000", authentication::session::tests::CODE)).await.expect("website session") else { panic!("website login"); };
            let actor = services.current().authenticate(Proof::Cookie { id: website.id(), token: website.token().text().parse().expect("website token") }).await.expect("website actor");
            let operation = Uuid::new_v4();
            let service = services.authentication().clone();
            let mut worker = tokio::spawn(async move { service.create_mobile_session(input(operation, "revoked-before-readback")).await });
            request(&mut supplier, &mut worker).await.reply(200, r#"{"access_token":"fixture-token","expires_in":7200}"#.into());
            let phone = request(&mut supplier, &mut worker).await;
            proxy.arm(Fault::LostCommitBeforeSessionRead);
            phone.reply(200, serde_json::json!({"errcode":0,"phone_info":{"phoneNumber":"8613800000000","purePhoneNumber":"13800000000","countryCode":"86","watermark":{"appid":"mini-fixture"}}}).to_string());
            proxy.intercepted().await;
            let logout = services.revocation().revoke(actor, Uuid::new_v4().to_string().parse().expect("logout operation"), crate::identity::revocation::Scope::All).await.expect("website revokes both sources");
            assert_eq!(logout.count().get(), 2);
            proxy.release();
            let error = worker.await.expect("worker").err().expect("revoked handoff");
            assert_eq!(error.failure(), authentication::Failure::Verification, "a verified unavailable result is not an unknown commit: {error:?}");
            assert!(matches!(error, authentication::Error::SessionRefused { .. }));
            services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await; proxy.stop().await;
        }).await.expect("known refusal race budget");
    });
}
