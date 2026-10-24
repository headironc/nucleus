use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*};

#[test]
fn mobile_transfer_http_binds_current_phone_and_replays_without_new_effects() {
    use fixture::database_proxy::{Fault, Proxy};

    const FAILED_OPERATION: &str = "00000000-0000-4000-9000-000000000104";
    if std::env::var_os("NUCLEUS_TEST_TRANSFER_DIAGNOSTICS").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let child = tokio::process::Command::new(
                std::env::current_exe().expect("test executable"),
            )
            .args([
                "--exact",
                "mobile::mobile_transfer_http_binds_current_phone_and_replays_without_new_effects",
                "--nocapture",
            ])
            .env("NUCLEUS_TEST_TRANSFER_DIAGNOSTICS", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("isolated diagnostics child");
            let output = timeout(Duration::from_secs(40), child.wait_with_output())
                .await
                .expect("child budget")
                .expect("child completion");
            let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
            assert!(output.status.success(), "{text}");
            let events: Vec<serde_json::Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).expect("JSON event"))
                .collect();
            let failures: Vec<_> = events
                .iter()
                .filter(|event| event["level"] == "ERROR")
                .collect();
            assert_eq!(
                failures.len(),
                1,
                "one final diagnostic for the actual rollback failure"
            );
            let failure = failures[0];
            assert_eq!(failure["fields"]["stage"], "transfer_completion");
            assert_eq!(failure["fields"]["operation_id"], FAILED_OPERATION);
            assert_eq!(failure["fields"]["kind"], "Unavailable");
            let cause = failure["fields"]["error"]
                .as_str()
                .expect("structured cause");
            assert!(cause.contains("Rollback") && cause.contains("Verification"));
            for secret in [
                "13900000000",
                "13800000000",
                "private-invalid-value",
                "mobile:",
                "__Host-nucleus-session-",
            ] {
                assert!(
                    !events.iter().any(|event| contains_text(event, secret)),
                    "private input leaked"
                );
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("actual online diagnostics");
    isolated_child(|| {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let name = database(&server).await;
                let supplier = Supplier::echoing().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let application = Application::start(
                    proxy_connections(&name, &proxy), keys(0),
                    services(8).expect("settings"),
                    process::Settings::new(Duration::from_secs(5)).expect("stop"),
                    // Keep the held SMS response alive across the verification and replay transactions.
                    supplier.client(), message_with_timeout(sms.address(), sms.authority(), 10_000),
                ).await.expect("verified startup");
                let issuance = application.registration().clone();
                let (base, stop, task) = running(application).await;
                let actor = issued_session_for(&base, &mut sms, "13900000000").await;
                age_issuance(&server, &name, &issuance).await;
                let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("explicit Origin client");
                let sending = Uuid::new_v4();
                let token = format!("mobile:{sending}");
                let request = |path: &str, operation: Uuid, source: u8| client.post(format!("{base}{path}"))
                    .header("operation-id", operation.to_string()).header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("x-real-ip", format!("203.0.149.{source}"));
                let body = serde_json::json!({"mobile":"13900000000", "token":token});
                for (path, input, field) in [
                    ("/api/v1/accounts/current/codes", body.clone(), "mobile"),
                    ("/api/v1/mobile-changes", serde_json::json!({"messageId":Uuid::new_v4(), "code":"000000"}), "messageId"),
                ] {
                    let mut invalid = input.clone(); invalid[field] = serde_json::json!("private-invalid-value");
                    let mut extra = input.clone(); extra["extra"] = serde_json::json!(true);
                    for (index, (builder, status)) in [
                        (request(path, sending, 1).json(&input), 403),
                        (request(path, sending, 2).header("origin", "https://other.example.com").json(&input), 403),
                        (request(path, sending, 3).header("origin", "https://example.com").header("sec-fetch-site", "cross-site").json(&input), 403),
                        (request(path, sending, 4).header("origin", "https://example.com").json(&invalid), 422),
                        (request(path, sending, 5).header("origin", "https://example.com").json(&extra), 422),
                        (request(path, sending, 6).header("origin", "https://example.com").header("content-type", "application/json").body("{"), 400),
                        (request(path, sending, 7).header("origin", "https://example.com").body(input.to_string()), 415),
                    ].into_iter().enumerate() {
                        let response = builder.send().await.expect("input refusal");
                        assert_eq!(response.status(), status, "{path} case {index}");
                        assert_eq!(response.headers()["cache-control"], "no-store");
                        assert!(!response.headers().contains_key("set-cookie"));
                        assert_eq!(response.json::<serde_json::Value>().await.expect("safe refusal"), serde_json::json!({"code":if status==403 {"permission_denied"} else {"invalid_input"}}));
                    }
                }
                for (index, code) in ["12345", "1234567", "12345678", "１２３４５６"].into_iter().enumerate() {
                    let response = request("/api/v1/mobile-changes", sending, 40 + u8::try_from(index).expect("four inputs"))
                        .header("origin", "https://example.com")
                        .json(&serde_json::json!({"messageId":Uuid::new_v4(), "code":code})).send().await.expect("invalid code shape");
                    assert_eq!(response.status(), 422);
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe rejection"), serde_json::json!({"code":"invalid_input"}));
                }
                let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.challenges WHERE purpose='mobile_change') AND NOT EXISTS(SELECT 1 FROM identity.transfers) AND NOT EXISTS(SELECT 1 FROM identity.rejections)", &[]).await.expect("refused inputs have no facts").get::<_, bool>(0));
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 1);
                let response = request("/api/v1/accounts/current/codes", sending, 10).header("origin", "https://example.com").json(&serde_json::json!({"mobile":"13800000000", "token":token})).send().await.expect("wrong current phone");
                assert_eq!(response.status(), 409);
                assert_eq!(response.json::<serde_json::Value>().await.expect("target refusal")["code"], "target_changed");
                let mut sending_task = tokio::spawn(request("/api/v1/accounts/current/codes", sending, 11).header("origin", "https://example.com").json(&body).send());
                let sent = tokio::select! {
                    result = &mut sending_task => panic!("send ended before supplier call: {:?}", result.expect("caller").expect("response").status()),
                    result = timeout(Duration::from_secs(5), sms.request()) => result.expect("actual SMS"),
                };
                let target = std::str::from_utf8(&sent.bytes).expect("wire").lines().next().expect("line").split(' ').nth(1).expect("target");
                let fields: std::collections::BTreeMap<String,String> = url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("URL").query_pairs().into_owned().collect();
                assert_eq!(fields["TemplateCode"], "change"); assert_eq!(fields["PhoneNumber"], "13900000000");
                assert_eq!(fields["CodeLength"], "6");
                let code = serde_json::from_str::<serde_json::Value>(&fields["TemplateParam"]).expect("parameters")["code"].as_str().expect("code").to_owned();
                assert_eq!(code.len(), 6); assert!(code.bytes().all(|digit| digit.is_ascii_digit()));
                let message_id = &fields["OutId"];
                let wrong = if code == "000000" { "000001" } else { "000000" };
                let rejected = Uuid::new_v4();
                let operation = Uuid::new_v4();
                let confirmation = serde_json::json!({"messageId":message_id, "code":code});
                let work = Box::pin(async {
                    for source in 20..22 {
                        let response = request("/api/v1/mobile-changes", rejected, source).header("origin", "https://example.com").json(&serde_json::json!({"messageId":message_id, "code":wrong})).send().await.expect("wrong code");
                        assert_eq!(response.status(), 403); assert_eq!(response.json::<serde_json::Value>().await.expect("refusal")["code"], "verification_invalid");
                    }
                    let response = request("/api/v1/mobile-changes", rejected, 22).header("origin", "https://example.com").json(&confirmation).send().await.expect("changed original code");
                    assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch")["code"], "request_mismatch");
                    let response = request("/api/v1/mobile-changes", operation, 23).header("origin", "https://example.com").json(&confirmation).send().await.expect("valid code while delivery is pending");
                    assert_eq!(response.status(), 201); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    let original = response.json::<serde_json::Value>().await.expect("transfer");
                    assert_eq!(original["code"], "ok"); assert_eq!(original["transfer"].as_object().expect("bounded result").len(), 2);
                    let fact = observer.client().query_one("SELECT transfer.id, transfer.created_at, transfer.session_id=$2::text::uuid AND message.status='sending' AND confirmation.failures=1 AND (SELECT count(*)=1 FROM identity.rejections WHERE message_id=message.id) AS bound FROM identity.transfers transfer JOIN identity.messages message ON message.id=transfer.message_id JOIN identity.confirmations confirmation ON confirmation.id=message.id WHERE transfer.operation_id=$1", &[&operation, &actor.0]).await.expect("independent committed result before replay or delivery response");
                    assert!(fact.get::<_,bool>("bound")); assert_eq!(original["transfer"]["id"], fact.get::<_,Uuid>("id").to_string());
                    let expiry = chrono::DateTime::parse_from_rfc3339(original["transfer"]["expiresAt"].as_str().expect("expiry")).expect("timestamp");
                    assert_eq!(expiry, fact.get::<_,chrono::DateTime<chrono::Utc>>("created_at") + chrono::TimeDelta::minutes(10));
                    let response = request("/api/v1/mobile-changes", operation, 24).header("origin", "https://example.com").json(&confirmation).send().await.expect("original result");
                    assert_eq!(response.status(), 201); assert_eq!(response.json::<serde_json::Value>().await.expect("replay"), original);
                    let response = request("/api/v1/mobile-changes", Uuid::new_v4(), 25).header("origin", "https://example.com").json(&confirmation).send().await.expect("consumed proof");
                    assert_eq!(response.status(), 410); assert_eq!(response.json::<serde_json::Value>().await.expect("new verification required")["code"], "verification_required");
                    accept_sms(sent);
                    let response = sending_task.await.expect("sender ended").expect("message response");
                    assert_eq!(response.status(), 200);
                    let message = response.json::<serde_json::Value>().await.expect("sent message");
                    assert_eq!(message["message"]["id"], *message_id); assert_eq!(message["message"]["delivery"], "sent");
                    let response = request("/api/v1/accounts/current/codes", sending, 12).header("origin", "https://example.com").json(&body).send().await.expect("message replay");
                    assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("same message"), message);
                    proxy.arm(Fault::RejectedRollback);
                    let failed_operation = FAILED_OPERATION.parse().expect("failure operation");
                    let response = request("/api/v1/mobile-changes", failed_operation, 26).header("origin", "https://example.com").json(&confirmation).send().await.expect("refusal with failed rollback");
                    assert_eq!(response.status(), 503);
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe dependency error")["code"], "dependency_unavailable");
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual ROLLBACK fault");
                    assert!(observer.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM identity.transfers WHERE operation_id=$1) AND NOT EXISTS (SELECT 1 FROM identity.rejections WHERE operation_id=$1)", &[&failed_operation]).await.expect("failed refusal has no partial result before retry").get::<_,bool>(0));
                    let response = request("/api/v1/mobile-changes", failed_operation, 27).header("origin", "https://example.com").json(&confirmation).send().await.expect("fresh refusal after cleanup");
                    assert_eq!(response.status(), 410);
                    assert_eq!(response.json::<serde_json::Value>().await.expect("ordinary refusal")["code"], "verification_required");
                    let document = http_client().get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("schema");
                    for path in ["/api/v1/accounts/current/codes", "/api/v1/mobile-changes"] {
                        let parameters = document["paths"][path]["post"]["parameters"].as_array().expect("documented route");
                        for name in ["Operation-Id", "Session-Id", "Origin"] {
                            assert!(parameters.iter().any(|parameter| parameter["name"]==name && parameter["required"]==true));
                        }
                    }
                    let input = &document["components"]["schemas"]["transfer.Input"];
                    assert_eq!(input["additionalProperties"], false); assert_eq!(input["properties"]["code"]["pattern"], "^[0-9]{6}$");
                    let reference = input["properties"]["messageId"]["$ref"].as_str().expect("owned message ID");
                    assert_eq!(document.pointer(reference.strip_prefix('#').expect("local reference")).expect("ID schema")["format"], "uuid");
                    stop.send(()).expect("stop"); let (application, ended) = task.await.expect("server ended"); ended.expect("drained"); drop(application);
                });
                tokio::select! { biased;
                    _ = sms.request() => panic!("no extra SMS during transfer, replay or drain"),
                    () = work => {},
                }
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 2, "only authentication and current-phone verification");
                observer.terminate().await.expect("observer ended"); sms.stop().await; proxy.stop().await;
            })).await.expect("bounded mobile transfer HTTP verification");
        }));
    });
}

#[test]
fn mobile_binding_http_checks_input_and_sends_to_an_occupied_new_phone() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(30), Box::pin(async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(
                connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),
                supplier.client(), message_at(sms.address(), sms.authority()),
            ).await.expect("verified startup");
            let issuance = application.registration().clone();
            let (base, stop, task) = running(application).await;
            let actor = issued_session_for(&base, &mut sms, "13900000000").await;
            let occupied = issued_session_from(&base, &mut sms, "13800000000", "203.0.150.1").await;
            age_issuance(&server, &name, &issuance).await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("explicit Origin client");
            let request = |path: &str, operation: Uuid, source: u8| client.post(format!("{base}{path}"))
                .header("operation-id", operation.to_string()).header("session-id", &actor.0).header("cookie", &actor.1)
                .header("x-real-ip", format!("203.0.150.{source}"));
            let operation = Uuid::new_v4();
            let mut sending = tokio::spawn(request("/api/v1/accounts/current/codes", operation, 2).header("origin", "https://example.com").json(&serde_json::json!({"mobile":"13900000000", "token":format!("mobile:{operation}")})).send());
            let sent = tokio::select! {
                result = &mut sending => panic!("sender ended before SMS: {:?}", result.expect("caller").expect("response").status()),
                request = sms.request() => request,
            };
            let target = std::str::from_utf8(&sent.bytes).expect("wire").lines().next().expect("line").split(' ').nth(1).expect("target");
            let fields: std::collections::BTreeMap<String,String> = url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("URL").query_pairs().into_owned().collect();
            let code = serde_json::from_str::<serde_json::Value>(&fields["TemplateParam"]).expect("parameters")["code"].as_str().expect("code").to_owned();
            accept_sms(sent);
            assert_eq!(sending.await.expect("caller").expect("message response").status(), 200);
            let response = request("/api/v1/mobile-changes", Uuid::new_v4(), 3).header("origin", "https://example.com").json(&serde_json::json!({"messageId":fields["OutId"],"code":code})).send().await.expect("create transfer");
            assert_eq!(response.status(), 201);
            let transfer = response.json::<serde_json::Value>().await.expect("transfer");
            let id = transfer["transfer"]["id"].as_str().expect("transfer ID");
            let path = format!("/api/v1/mobile-changes/{id}/codes");
            let operation = Uuid::new_v4();
            let body = serde_json::json!({"mobile":"13800000000","token":format!("binding:{operation}")});
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent runtime observer");
            let refusals = Box::pin(async {
                for (builder, status, code) in [
                    (request(&path, operation, 4).json(&body), 403, "permission_denied"),
                    (request(&path, operation, 5).header("origin", "https://other.example.com").json(&body), 403, "permission_denied"),
                    (request(&path, operation, 6).header("origin", "https://example.com").json(&serde_json::json!({"mobile":"13900000000","token":"valid-token"})), 409, "state_conflict"),
                    (request(&path, operation, 7).header("origin", "https://example.com").json(&serde_json::json!({"mobile":"invalid","token":"valid-token"})), 422, "invalid_input"),
                    (request(&path, operation, 8).header("origin", "https://example.com").json(&serde_json::json!({"mobile":"13800000000","token":"valid-token","extra":true})), 422, "invalid_input"),
                    (request("/api/v1/mobile-changes/not-a-uuid/codes", operation, 9).header("origin", "https://example.com").json(&body), 400, "invalid_input"),
                    (request(&format!("/api/v1/mobile-changes/{}/codes", Uuid::new_v4()), operation, 10).header("origin", "https://example.com").json(&body), 410, "verification_required"),
                    (client.post(format!("{base}{path}")).header("operation-id", operation.to_string()).header("x-real-ip", "203.0.150.11").header("origin", "https://example.com").header("session-id", &occupied.0).header("cookie", &occupied.1).json(&body), 410, "verification_required"),
                    (client.post(format!("{base}{path}")).header("operation-id", operation.to_string()).header("origin", "https://example.com").header("x-real-ip", "203.0.150.12").json(&body), 401, "authentication_invalid"),
                ] {
                    let response = builder.send().await.expect("input or qualification refusal");
                    assert_eq!(response.status(), status); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe error"), serde_json::json!({"code":code}));
                }
                assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.challenges WHERE purpose='mobile_binding')", &[]).await.expect("refusals left no new facts").get::<_,bool>(0));
            });
            tokio::select! { biased;
                _ = sms.request() => panic!("no SMS for invalid inputs"),
                () = refusals => {},
            }
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 3);
            let mut sending = tokio::spawn(request(&path, operation, 13).header("origin", "https://example.com").json(&body).send());
            let sent = tokio::select! {
                result = &mut sending => panic!("binding ended before SMS: {:?}", result.expect("caller").expect("response").status()),
                request = sms.request() => request,
            };
            let target = std::str::from_utf8(&sent.bytes).expect("wire").lines().next().expect("line").split(' ').nth(1).expect("target");
            let fields: std::collections::BTreeMap<String,String> = url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("URL").query_pairs().into_owned().collect();
            assert_eq!(fields["PhoneNumber"], "13800000000"); assert_eq!(fields["TemplateCode"], "bind");
            assert_eq!(fields["CodeLength"], "6");
            let code = serde_json::from_str::<serde_json::Value>(&fields["TemplateParam"]).expect("parameters")["code"].as_str().expect("code").to_owned();
            assert_eq!(code.len(), 6); assert!(code.bytes().all(|digit| digit.is_ascii_digit()));
            accept_sms(sent);
            let verification = Box::pin(async {
                let response = sending.await.expect("caller").expect("binding result");
                assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie"));
                let original = response.json::<serde_json::Value>().await.expect("message");
                assert_eq!(original["message"]["id"], fields["OutId"]); assert_eq!(original["message"]["delivery"], "sent");
                assert!(observer.client().query_one("SELECT challenge.transfer_id=$1::text::uuid AND challenge.session_id=$2::text::uuid AND (challenge.mobile).hash=(account.mobile).hash AND account.person_id<>challenge.person_id AND message.status='sent' FROM identity.challenges challenge JOIN identity.messages message ON message.challenge_id=challenge.id JOIN identity.sessions session ON session.id=$3::text::uuid JOIN identity.accounts account ON account.id=session.account_id WHERE challenge.operation_id=$4", &[&id, &actor.0, &occupied.0, &operation]).await.expect("new phone actually occupied by another account, without blocking verification").get::<_,bool>(0));
                let response = request(&path, operation, 14).header("origin", "https://example.com").json(&body).send().await.expect("original readback");
                assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("same message"), original);
                let response = request(&format!("/api/v1/mobile-changes/{}/codes", Uuid::new_v4()), operation, 15).header("origin", "https://example.com").json(&body).send().await.expect("different transfer");
                assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch")["code"], "request_mismatch");
                let document = http_client().get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("schema");
                let endpoint = &document["paths"]["/api/v1/mobile-changes/{id}/codes"]["post"];
                let parameters = endpoint["parameters"].as_array().expect("documented endpoint");
                for name in ["id", "Operation-Id", "Session-Id", "Origin"] {
                    assert!(parameters.iter().any(|parameter| parameter["name"]==name && parameter["required"]==true));
                }
                let id = parameters.iter().find(|parameter| parameter["name"]=="id").expect("path ID");
                let reference = id["schema"]["$ref"].as_str().expect("owned transfer ID");
                assert_eq!(document.pointer(reference.strip_prefix('#').expect("local reference")).expect("ID schema")["format"], "uuid");
                assert!(endpoint["responses"]["409"]["description"].as_str().expect("conflict code").contains("state_conflict"));
                stop.send(()).expect("stop"); let (application, ended) = task.await.expect("server ended"); ended.expect("drained"); drop(application);
            });
            tokio::select! { biased;
                _ = sms.request() => panic!("no additional SMS through replay and drain"),
                () = verification => {},
            }
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 4, "one verification per actual sending operation");
            observer.terminate().await.expect("reader ended"); sms.stop().await;
        })).await.expect("bounded new-phone HTTP flow");
    }));
}

#[test]
fn mobile_completion_http_replaces_the_phone_and_reads_the_outcome_with_a_new_session() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(35), Box::pin(async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let issuance = application.registration().clone();
            let (base, stop, task) = running(application).await;
            let original = issued_session_for(&base, &mut sms, "13900000000").await;
            age_issuance(&server, &name, &issuance).await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("explicit origin client");
            let request = |method: reqwest::Method, path: &str, operation: Uuid, source: u8| client.request(method, format!("{base}{path}"))
                .header("operation-id", operation.to_string()).header("session-id", &original.0).header("cookie", &original.1).header("x-real-ip", format!("203.0.151.{source}"));
            let sending_id = Uuid::new_v4();
            let current = mobile_code(request(reqwest::Method::POST, "/api/v1/accounts/current/codes", sending_id, 1).header("origin", "https://example.com").json(&serde_json::json!({"mobile":"13900000000","token":format!("mobile:{sending_id}")})), &mut sms).await;
            let response = request(reqwest::Method::POST, "/api/v1/mobile-changes", Uuid::new_v4(), 2).header("origin", "https://example.com").json(&serde_json::json!({"messageId":current.0,"code":current.1})).send().await.expect("establish transfer");
            assert_eq!(response.status(), 201);
            let body = response.json::<serde_json::Value>().await.expect("transfer");
            let id = body["transfer"]["id"].as_str().expect("transfer ID").to_owned();
            let path = format!("/api/v1/mobile-changes/{id}");
            let sending_id = Uuid::new_v4();
            let binding = mobile_code(request(reqwest::Method::POST, &format!("{path}/codes"), sending_id, 3).header("origin", "https://example.com").json(&serde_json::json!({"mobile":"13800000000","token":format!("binding:{sending_id}")})), &mut sms).await;
            let operation = Uuid::new_v4();
            let input = serde_json::json!({"mobile":"13800000000","messageId":binding.0,"code":binding.1});
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent runtime observer");
            let work = Box::pin(async {
                let response = request(reqwest::Method::GET, &path, Uuid::new_v4(), 4).header("sec-fetch-site", "same-origin").send().await.expect("pending status");
                assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("status")["transfer"]["status"], "pending");
                for (path, expected) in [(format!("{path}?extra=1"),400), ("/api/v1/mobile-changes/not-a-uuid".into(),400), (format!("/api/v1/mobile-changes/{}",Uuid::new_v4()),404)] {
                    let response = request(reqwest::Method::GET, &path, Uuid::new_v4(), 5).header("sec-fetch-site", "same-origin").send().await.expect("read refusal");
                    assert_eq!(response.status(), expected);
                }
                let response = request(reqwest::Method::HEAD, &path, Uuid::new_v4(), 6).header("sec-fetch-site", "same-origin").send().await.expect("HEAD");
                assert_eq!(response.status(), 200); assert!(response.bytes().await.expect("no HEAD body").is_empty());
                for (builder, expected, expected_code) in [
                    (request(reqwest::Method::PUT, "/api/v1/accounts/current/mobile", operation, 7).json(&input),403,"permission_denied"),
                    (request(reqwest::Method::PUT, "/api/v1/accounts/current/mobile", operation, 8).header("origin","https://other.example.com").json(&input),403,"permission_denied"),
                    (request(reqwest::Method::PUT, "/api/v1/accounts/current/mobile", operation, 9).header("origin","https://example.com").json(&serde_json::json!({"mobile":"invalid","messageId":binding.0,"code":binding.1})),422,"invalid_input"),
                    (request(reqwest::Method::PUT, "/api/v1/accounts/current/mobile", operation, 10).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800000000","messageId":"invalid","code":binding.1})),422,"invalid_input"),
                    (request(reqwest::Method::PUT, "/api/v1/accounts/current/mobile", operation, 11).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800000000","messageId":binding.0,"code":"12345678"})),422,"invalid_input"),
                    (request(reqwest::Method::PUT, "/api/v1/accounts/current/mobile", operation, 12).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800000000","messageId":binding.0,"code":binding.1,"extra":true})),422,"invalid_input"),
                    (request(reqwest::Method::PUT, "/api/v1/accounts/current/mobile", operation, 13).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13900000000","messageId":binding.0,"code":binding.1})),409,"state_conflict"),
                ] {
                    let response = builder.send().await.expect("input refusal");
                    assert_eq!(response.status(), expected); assert_eq!(response.headers()["cache-control"], "no-store");
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure"), serde_json::json!({"code":expected_code}));
                }
                let response = request(reqwest::Method::PUT, "/api/v1/accounts/current/mobile", operation, 14).header("origin", "https://example.com").json(&input).send().await.expect("complete mobile change");
                assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie"));
                let completed = response.json::<serde_json::Value>().await.expect("completion");
                assert_eq!(completed["code"],"ok"); assert_eq!(completed["completion"]["id"],id); assert!(completed["completion"]["completedAt"].is_string());
                assert!(observer.client().query_one("SELECT completion.id=$1::text::uuid AND completion.operation_id=$2 AND completion.session_id=$3::text::uuid AND completion.message_id=$4::text::uuid AND completion.sessions=ARRAY[session.id] AND completion.sequence=2 AND account.sequence=completion.sequence AND account.version=2 AND account.changed_at=completion.completed_at AND person.sequence=1 AND session.sequence=2 AND session.version=2 AND session.revoked_at=completion.completed_at AND revocation.sequence=2 AND revocation.mode='transfer' AND revocation.cause_id=completion.id AND revocation.operation_id=completion.operation_id AND revocation.revoked_at=completion.completed_at AND revocation.transaction_id=completion.transaction_id AND NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=session.id) FROM identity.replacements completion JOIN identity.accounts account ON account.id=completion.account_id JOIN identity.persons person ON person.id=account.person_id JOIN identity.sessions session ON session.id=completion.session_id AND session.account_id=account.id JOIN identity.revocations revocation ON revocation.id=session.id AND revocation.account_id=account.id WHERE completion.id=$1::text::uuid", &[&id,&operation,&original.0,&binding.0]).await.expect("persistent outcome before replay").get::<_,bool>(0));
                for builder in [request(reqwest::Method::PUT, "/api/v1/accounts/current/mobile", operation, 15).header("origin","https://example.com").json(&input), request(reqwest::Method::GET,&path,Uuid::new_v4(),16).header("sec-fetch-site","same-origin")] {
                    let response=builder.send().await.expect("old session refusal"); assert_eq!(response.status(),401); assert_eq!(response.json::<serde_json::Value>().await.expect("failure")["code"],"authentication_invalid");
                }
            });
            tokio::select! { biased; _=sms.request()=>panic!("completion and read never send SMS"), ()=work=>{} }
            age_issuance(&server, &name, &issuance).await;
            let owner = connections(&server, &name, "postgres").recover().await.expect("isolated remaining historical facts");
            owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE identity.transfers SET created_at=created_at-interval '61 seconds'; UPDATE identity.confirmations SET expires_at=expires_at-interval '61 seconds'; COMMIT").await.expect("preserve complete fixture timeline");
            owner.terminate().await.expect("owner ended");
            let current=issued_session_from(&base,&mut sms,"13800000000","203.0.151.17").await;
            let work=Box::pin(async {
                let response=client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&current.0).header("cookie",&current.1).header("x-real-ip","203.0.151.18").header("sec-fetch-site","same-origin").send().await.expect("new-session result");
                assert_eq!(response.status(),200); assert_eq!(response.headers()["cache-control"],"no-store"); assert_eq!(response.json::<serde_json::Value>().await.expect("original result")["transfer"]["status"],"completed");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.accounts",&[]).await.expect("same retained account").get::<_,i64>(0),1);
                let document=http_client().get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("schema");
                let put=&document["paths"]["/api/v1/accounts/current/mobile"]["put"];
                assert!(put["parameters"].as_array().expect("PUT parameters").iter().any(|value|value["name"]=="Origin" && value["required"]==true));
                assert!(document["paths"]["/api/v1/mobile-changes/{id}"]["get"]["parameters"].as_array().expect("GET parameters").iter().all(|value|value["name"]!="Origin"));
                assert_eq!(document["components"]["schemas"]["transfer.CompletionInput"]["additionalProperties"],false);
                assert_eq!(document["paths"]["/api/v1/mobile-changes/{id}"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/transfer.Status");
                assert_eq!(document["components"]["schemas"]["transfer.Status"]["properties"]["transfer"]["$ref"],"#/components/schemas/transfer.State");
                let variants=document["components"]["schemas"]["transfer.State"]["oneOf"].as_array().expect("documented outcomes");
                for (status,field) in [("pending","expiresAt"),("completed","completedAt")] {
                    let variant=variants.iter().find(|variant|variant["properties"]["status"]["enum"]==serde_json::json!([status])).expect("documented outcome");
                    assert_eq!(variant["properties"][field]["format"],"date-time");
                    assert!(variant["required"].as_array().expect("required outcome fields").contains(&serde_json::json!(field)));
                }
                stop.send(()).expect("stop"); let (application,ended)=task.await.expect("server ended"); ended.expect("drained"); drop(application);
            });
            tokio::select! { biased; _=sms.request()=>panic!("new-session read and drain never send SMS"), ()=work=>{} }
            assert_eq!(supplier.calls.load(Ordering::SeqCst),4);
            observer.terminate().await.expect("observer ended"); sms.stop().await;
        })).await.expect("bounded mobile completion HTTP lifecycle");
    }));
}

#[test]
fn mobile_status_http_discloses_only_authorized_results_and_reports_read_failure_once() {
    const FAILED_OPERATION: &str = "00000000-0000-4000-9000-000000000105";
    if std::env::var_os("NUCLEUS_TEST_MOBILE_STATUS_DIAGNOSTICS").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "mobile::mobile_status_http_discloses_only_authorized_results_and_reports_read_failure_once", "--nocapture"])
                .env("NUCLEUS_TEST_MOBILE_STATUS_DIAGNOSTICS", "1")
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
                .kill_on_drop(true).spawn().expect("isolated diagnostics child");
            let output = timeout(Duration::from_secs(40), child.wait_with_output()).await.expect("child budget").expect("child result");
            let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
            assert!(output.status.success(), "{text}");
            let events: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).expect("JSON event")).collect();
            let failures: Vec<_> = events.iter().filter(|event| event["level"] == "ERROR").collect();
            assert_eq!(failures.len(), 1, "one final read failure");
            assert_eq!(failures[0]["fields"]["stage"], "transfer_read");
            assert_eq!(failures[0]["fields"]["operation_id"], FAILED_OPERATION);
            assert!(failures[0]["fields"]["error"].as_str().expect("structured safe error").contains("Query"));
            for secret in ["13900000000", "13800000000", "13700000000", "mobile:", "__Host-nucleus-session-"] {
                assert!(!events.iter().any(|event| contains_text(event, secret)), "private input leaked");
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("actual online diagnostics");
    isolated_child(|| {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let name = database(&server).await;
                let supplier = Supplier::echoing().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                    services(8).expect("settings"),
                    process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
                let issuance = application.registration().clone();
                let (base, stop, task) = running(application).await;
                let manager = issued_session_from(&base, &mut sms, "13900000000", "203.0.152.1").await;
                let target = issued_session_from(&base, &mut sms, "13800000000", "203.0.152.2").await;
                let other = issued_session_with_identity(&base, &mut sms, "13700000000", "203.0.152.3", "000000200002290029").await;
                age_issuance(&server, &name, &issuance).await;
                let client = http_client();
                let sending = Uuid::new_v4();
                let code = mobile_code(client.post(format!("{base}/api/v1/accounts/current/codes"))
                    .header("operation-id", sending.to_string()).header("session-id", &target.0).header("cookie", &target.1).header("x-real-ip", "203.0.152.4")
                    .json(&serde_json::json!({"mobile":"13800000000","token":format!("mobile:{sending}")})), &mut sms).await;
                let response = client.post(format!("{base}/api/v1/mobile-changes")).header("operation-id", Uuid::new_v4().to_string())
                    .header("session-id", &target.0).header("cookie", &target.1).header("x-real-ip", "203.0.152.5")
                    .json(&serde_json::json!({"messageId":code.0,"code":code.1})).send().await.expect("real target transfer");
                assert_eq!(response.status(), 201);
                let created = response.json::<serde_json::Value>().await.expect("transfer");
                let id = created["transfer"]["id"].as_str().expect("transfer ID");
                let path = format!("/api/v1/mobile-changes/{id}");
                let request = |actor: &(String, String), method: reqwest::Method, path: &str, source: u8| client.request(method, format!("{base}{path}"))
                    .header("operation-id", Uuid::new_v4().to_string()).header("session-id", &actor.0).header("cookie", &actor.1).header("x-real-ip", format!("203.0.152.{source}"));
                let work = Box::pin(async {
                    for (actor, source) in [(&manager, 6), (&target, 7)] {
                        let response = request(actor, reqwest::Method::GET, &path, source).send().await.expect("authorized read");
                        assert_eq!(response.status(), 200);
                        assert_eq!(response.headers()["cache-control"], "no-store");
                        assert!(!response.headers().contains_key("set-cookie"));
                        assert_eq!(response.json::<serde_json::Value>().await.expect("minimal result"), serde_json::json!({"code":"ok","transfer":{"status":"pending","expiresAt":created["transfer"]["expiresAt"]}}));
                    }
                    for (actor, path, source) in [(&other, path.clone(), 8), (&manager, format!("/api/v1/mobile-changes/{}", Uuid::new_v4()), 9)] {
                        let response = request(actor, reqwest::Method::GET, &path, source).send().await.expect("hidden target");
                        assert_eq!(response.status(), 404);
                        assert_eq!(response.json::<serde_json::Value>().await.expect("same hidden outcome"), serde_json::json!({"code":"target_unavailable"}));
                    }
                    for (actor, source, expected) in [(&manager, 10, 200), (&other, 11, 404)] {
                        let response = request(actor, reqwest::Method::HEAD, &path, source).send().await.expect("HEAD");
                        assert_eq!(response.status(), expected);
                        assert!(response.bytes().await.expect("empty HEAD body").is_empty());
                    }
                    for (builder, expected) in [
                        (request(&manager, reqwest::Method::GET, &format!("{path}?extra=1"), 12), 400),
                        (request(&manager, reqwest::Method::GET, &path, 13).body("{}"), 400),
                        (request(&manager, reqwest::Method::GET, &path, 14).header("origin", "https://other.example.com"), 403),
                    ] {
                        assert_eq!(builder.send().await.expect("read boundary").status(), expected);
                    }
                    let owner = connections(&server, &name, "postgres").recover().await.expect("isolated query fault");
                    owner.client().batch_execute("REVOKE SELECT ON identity.transfers FROM clerk").await.expect("deny result query");
                    let response = client.get(format!("{base}{path}")).header("operation-id", FAILED_OPERATION)
                        .header("session-id", &manager.0).header("cookie", &manager.1).header("x-real-ip", "203.0.152.15")
                        .send().await.expect("read failure");
                    assert_eq!(response.status(), 503);
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure"), serde_json::json!({"code":"dependency_unavailable"}));
                    owner.client().batch_execute("GRANT SELECT ON identity.transfers TO clerk").await.expect("restore disposable grant");
                    assert_eq!(request(&manager, reqwest::Method::GET, &path, 16).send().await.expect("ordinary failure preserves service").status(), 200);
                    assert!(owner.client().query_one("SELECT (SELECT count(*) FROM identity.transfers)=1 AND NOT EXISTS(SELECT 1 FROM identity.replacements) AND NOT EXISTS(SELECT 1 FROM identity.revocations)", &[]).await.expect("read-only effects").get::<_,bool>(0));
                    owner.terminate().await.expect("owner ended");
                    stop.send(()).expect("stop"); let (application, ended) = task.await.expect("HTTP ended"); ended.expect("drained"); drop(application);
                });
                tokio::select! { biased; _ = sms.request() => panic!("status reads and drain never send SMS"), () = work => {} }
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 4);
                sms.stop().await;
            })).await.expect("bounded management HTTP status");
        }));
    });
}

#[test]
fn assisted_mobile_http_requires_explicit_confirmation_and_preserves_the_manager_cookie() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(35), Box::pin(async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let manager = issued_session_from(&base, &mut sms, "13900000000", "203.0.153.1").await;
            let target = issued_session_from(&base, &mut sms, "13800000000", "203.0.153.2").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("explicit origin client");
            let request = |actor: &(String, String), method: reqwest::Method, path: &str, operation: Uuid, source: u8| client.request(method, format!("{base}{path}"))
                .header("operation-id", operation.to_string()).header("session-id", &actor.0).header("cookie", &actor.1).header("x-real-ip", format!("203.0.153.{source}"));
            let response = request(&manager, reqwest::Method::POST, "/api/v1/accounts/candidates", Uuid::new_v4(), 3).header("origin", "https://example.com").json(&serde_json::json!({"mobile":"13800000000"})).send().await.expect("confirmed target");
            assert_eq!(response.status(), 200);
            let candidate = response.json::<serde_json::Value>().await.expect("candidate");
            let account_id = candidate["identity"]["accountId"].as_str().expect("account ID");
            let establish_path = format!("/api/v1/accounts/{account_id}/mobile-changes");
            let complete_path = format!("/api/v1/accounts/{account_id}/mobile");
            let input = serde_json::json!({"personId":candidate["identity"]["personId"], "revisionId":candidate["identity"]["revisionId"], "mobile":"13800000000", "verified":true});
            let operation = Uuid::new_v4();
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
            let work = Box::pin(async {
                let mut invalid = input.clone(); invalid["verified"] = serde_json::json!(false);
                let mut absent = input.clone(); absent.as_object_mut().expect("object").remove("verified");
                let mut malformed = input.clone(); malformed["personId"] = serde_json::json!("invalid");
                let mut extra = input.clone(); extra["extra"] = serde_json::json!(true);
                let mut changed = input.clone(); changed["revisionId"] = serde_json::json!(Uuid::new_v4());
                for (builder, status, code) in [
                    (request(&manager, reqwest::Method::POST, &establish_path, operation, 4).json(&input),403,"permission_denied"),
                    (request(&manager, reqwest::Method::POST, &establish_path, operation, 5).header("origin","https://other.example.com").json(&input),403,"permission_denied"),
                    (request(&manager, reqwest::Method::POST, "/api/v1/accounts/invalid/mobile-changes", operation, 6).header("origin","https://example.com").json(&input),400,"invalid_input"),
                    (request(&manager, reqwest::Method::POST, &establish_path, operation, 7).header("origin","https://example.com").json(&invalid),422,"invalid_input"),
                    (request(&manager, reqwest::Method::POST, &establish_path, operation, 8).header("origin","https://example.com").json(&absent),422,"invalid_input"),
                    (request(&manager, reqwest::Method::POST, &establish_path, operation, 9).header("origin","https://example.com").json(&malformed),422,"invalid_input"),
                    (request(&manager, reqwest::Method::POST, &establish_path, operation, 10).header("origin","https://example.com").json(&extra),422,"invalid_input"),
                    (request(&manager, reqwest::Method::POST, &establish_path, operation, 11).header("origin","https://example.com").json(&changed),409,"target_changed"),
                    (request(&target, reqwest::Method::POST, &establish_path, operation, 12).header("origin","https://example.com").json(&input),403,"permission_denied"),
                ] {
                    let response = builder.send().await.expect("safe refusal");
                    assert_eq!(response.status(), status); assert_eq!(response.headers()["cache-control"], "no-store");
                    assert_eq!(response.json::<serde_json::Value>().await.expect("failure"), serde_json::json!({"code":code}));
                }
                assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.transfers)", &[]).await.expect("refusals leave no transfer").get::<_,bool>(0));
                let response = request(&manager, reqwest::Method::POST, &establish_path, operation, 13).header("origin", "https://example.com").json(&input).send().await.expect("establishment");
                assert_eq!(response.status(), 201); assert!(!response.headers().contains_key("set-cookie"));
                let established = response.json::<serde_json::Value>().await.expect("transfer");
                let response = request(&manager, reqwest::Method::POST, &establish_path, operation, 14).header("origin", "https://example.com").json(&input).send().await.expect("original");
                assert_eq!(response.status(), 201); assert_eq!(response.json::<serde_json::Value>().await.expect("same result"), established);
                established
            });
            let established = tokio::select! { biased; _=sms.request()=>panic!("establishment never sends SMS"), result=work=>result };
            let id = established["transfer"]["id"].as_str().expect("transfer ID");
            let sending_id = Uuid::new_v4();
            let binding = mobile_code(request(&manager, reqwest::Method::POST, &format!("/api/v1/mobile-changes/{id}/codes"), sending_id, 15).header("origin", "https://example.com").json(&serde_json::json!({"mobile":"13600000000","token":format!("binding:{sending_id}")})), &mut sms).await;
            let operation = Uuid::new_v4();
            let input = serde_json::json!({"mobile":"13600000000", "messageId":binding.0, "code":binding.1});
            let work = Box::pin(async {
                let response = request(&manager, reqwest::Method::PUT, &complete_path, operation, 16).json(&input).send().await.expect("Origin required");
                assert_eq!(response.status(), 403);
                let response = request(&manager, reqwest::Method::PUT, &complete_path, operation, 17).header("origin", "https://example.com").json(&input).send().await.expect("assisted completion");
                assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                let completed = response.json::<serde_json::Value>().await.expect("completion"); assert_eq!(completed["completion"]["id"], id);
                assert!(observer.client().query_one("SELECT completion.account_id=$2::text::uuid AND completion.session_id=$3::text::uuid AND completion.sessions=ARRAY[session.id] AND completion.sequence=2 AND account.sequence=completion.sequence AND account.version=2 AND account.changed_at=completion.completed_at AND person.sequence=1 AND session.sequence=2 AND session.version=2 AND session.revoked_at=completion.completed_at AND revocation.sequence=2 AND revocation.mode='transfer' AND revocation.cause_id=completion.id AND revocation.operation_id=completion.operation_id AND revocation.revoked_at=completion.completed_at AND revocation.transaction_id=completion.transaction_id AND NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=session.id) AND EXISTS(SELECT 1 FROM identity.sessions manager WHERE manager.id=$3::text::uuid AND manager.sequence=1 AND manager.version=1 AND manager.revoked_at IS NULL) AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$3::text::uuid) FROM identity.replacements completion JOIN identity.accounts account ON account.id=completion.account_id JOIN identity.persons person ON person.id=account.person_id JOIN identity.sessions session ON session.id=$4::text::uuid AND session.account_id=account.id JOIN identity.revocations revocation ON revocation.id=session.id AND revocation.account_id=account.id WHERE completion.operation_id=$1", &[&operation,&account_id,&manager.0,&target.0]).await.expect("target-only effects before replay").get::<_,bool>(0));
                let response = request(&manager, reqwest::Method::PUT, &complete_path, operation, 18).header("origin", "https://example.com").json(&input).send().await.expect("manager remains authenticated");
                assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("original completion"), completed);
                let response = request(&target, reqwest::Method::GET, &format!("/api/v1/mobile-changes/{id}"), Uuid::new_v4(), 19).header("sec-fetch-site", "same-origin").send().await.expect("target old session"); assert_eq!(response.status(), 401);
                let document = http_client().get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("schema");
                for (path, method) in [("/api/v1/accounts/{id}/mobile-changes","post"),("/api/v1/accounts/{id}/mobile","put")] {
                    assert!(document["paths"][path][method]["parameters"].as_array().expect("parameters").iter().any(|parameter|parameter["name"]=="Origin" && parameter["required"]==true));
                }
                assert_eq!(document["components"]["schemas"]["transfer.Assistance"]["additionalProperties"], false);
                assert!(document["components"]["schemas"]["transfer.Assistance"]["required"].as_array().expect("required inputs").iter().any(|field|field=="verified"));
                stop.send(()).expect("stop"); let (application, ended) = task.await.expect("HTTP ended"); ended.expect("drained"); drop(application);
            });
            tokio::select! { biased; _=sms.request()=>panic!("completion, replay and drain never send SMS"), ()=work=>{} }
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 3);
            observer.terminate().await.expect("observer ended"); sms.stop().await;
        })).await.expect("bounded public assisted mobile flow");
    }));
}

pub(crate) async fn mobile_code(
    request: reqwest::RequestBuilder,
    sms: &mut fixture::supplier::Supplier,
) -> (String, String) {
    let mut task = tokio::spawn(request.send());
    let sent = tokio::select! {
        response = &mut task => panic!("code response preceded SMS: {}", response.expect("caller").expect("response").status()),
        request = sms.request() => request,
    };
    let target = std::str::from_utf8(&sent.bytes)
        .expect("wire")
        .lines()
        .next()
        .expect("line")
        .split(' ')
        .nth(1)
        .expect("target");
    let fields: std::collections::BTreeMap<String, String> =
        url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}"))
            .expect("URL")
            .query_pairs()
            .into_owned()
            .collect();
    let code = serde_json::from_str::<serde_json::Value>(&fields["TemplateParam"])
        .expect("parameters")["code"]
        .as_str()
        .expect("code")
        .to_owned();
    assert_eq!(fields["CodeLength"], "6");
    assert_eq!(code.len(), 6);
    assert!(code.bytes().all(|digit| digit.is_ascii_digit()));
    accept_sms(sent);
    let response = task.await.expect("caller").expect("code result");
    assert_eq!(response.status(), 200);
    let body = response.json::<serde_json::Value>().await.expect("message");
    assert_eq!(body["message"]["id"], fields["OutId"]);
    (fields["OutId"].clone(), code)
}
