use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, database::*, supplier::*, suspension::*};

#[test]
fn website_sms_session_creation_delivers_only_the_original_cookie_or_registration() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        for kind in ["session", "suspended", "registration"] {
            let name = empty_database(&server).await;
            let admin = connections(&server, &name, "postgres").recover().await.expect("isolated fixture");
            if kind == "registration" {
                let supplier = Supplier::new().await;
                let result = Application::start(connections(&server,&name,"clerk"), keys(0), services(8).expect("settings"), process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message().await).await;
                assert!(registration_role_missing(&result));
                assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            }
            {
                let operator_mobile = if kind == "session" { "13800000000" } else { "13900000000" };
                initialize(&server, &name, operator_mobile).await;
                if kind == "suspended" {
                    suspended_account(&server, &name).await;
                }
            }
            let now: chrono::DateTime<chrono::Utc> = admin.client().query_one("SELECT clock_timestamp()", &[]).await.expect("time").get(0);
            admin.terminate().await.expect("fixture ended");
            let supplier = Supplier::responding(serde_json::json!({"success":true,"challenge_ts":now,"hostname":"example.com","action":"send_authentication_code","cdata":OPERATION}).to_string()).await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let application = Application::start(proxy_connections(&name, &proxy), keys(0), services(8).expect("settings"), process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(),sms.authority())).await.expect("start");
            let (base, stop, task) = running(application).await;
            let client = http_client();
            let pending = tokio::spawn(client.post(format!("{base}/api/v1/codes")).header("operation-id", OPERATION).json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"})).send());
            let request = timeout(Duration::from_secs(4), sms.request()).await.expect("SMS fixture request");
            let text = std::str::from_utf8(&request.bytes).expect("wire");
            let target = text.lines().next().expect("line").split(' ').nth(1).expect("target");
            let url = url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("URL");
            let template = url.query_pairs().find(|(name, _)| name == "TemplateParam").expect("template").1.into_owned();
            let code = serde_json::from_str::<serde_json::Value>(&template).expect("template JSON")["code"].as_str().expect("code").to_owned();
            assert_eq!(code.len(), 6);
            assert!(code.bytes().all(|digit| digit.is_ascii_digit()));
            assert!(url.query_pairs().any(|(name, value)| name == "CodeLength" && value == "6"));
            accept_sms(request);
            let response = pending.await.expect("caller").expect("send");
            assert_eq!(response.status(), 200);
            let id = response.json::<serde_json::Value>().await.expect("message")["message"]["id"].as_str().expect("id").to_owned();
            let body = serde_json::json!({"secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","credential":{"type":"message","id":id,"mobile":"13800000000","code":code}});
            if kind == "registration" {
                let observer = connections(&server, &name, "clerk").recover().await.expect("runtime observer");
                let original: String = observer.client().query_one("SELECT row_to_json(verification)::text FROM identity.checks verification WHERE id=$1::text::uuid", &[&id]).await.expect("original check").get(0);
                for (field, invalid) in [("id", "malformed-message"), ("secret", "fictional-private-secret"), ("mobile", "+8613800000000"), ("code", "00000"), ("code", "0000000"), ("code", "00000000"), ("code", "１２３４５６")] {
                    let mut invalid_body = body.clone();
                    if field == "secret" { invalid_body[field] = invalid.into(); }
                    else { invalid_body["credential"][field] = invalid.into(); }
                    let response = client.post(format!("{base}/api/v1/sessions")).header("operation-id", OPERATION).header("x-real-ip", "192.0.2.2").json(&invalid_body).send().await.expect("invalid controlled value");
                    assert_eq!(response.status(), 422, "{field}");
                    assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe rejection"), serde_json::json!({"code":"invalid_input"}));
                    let actual: String = observer.client().query_one("SELECT row_to_json(verification)::text FROM identity.checks verification WHERE id=$1::text::uuid", &[&id]).await.expect("unchanged check").get(0);
                    assert_eq!(actual, original, "{field}");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.authentications", &[]).await.expect("no authentication").get::<_, i64>(0), 0);
                }
                observer.terminate().await.expect("observer ended");
            }
            let create_session = || client.post(format!("{base}/api/v1/sessions")).header("operation-id", OPERATION).json(&body);
            let response = create_session().header("origin", "https://attacker.invalid").send().await.expect("origin");
            assert_eq!(response.status(), 403);
            let response = create_session().send().await.expect("server-derived session delivery");
            let cookie = response.headers().get("set-cookie").cloned();
            let expected = if kind == "suspended" { 403 } else { 200 };
            assert_eq!(response.status(), expected);
            assert_eq!(response.headers()["cache-control"], "no-store");
            let original: serde_json::Value = response.json().await.expect("result");
            if kind == "session" {
                assert!(original.get("registration").is_none());
                let session = original["session"].as_object().expect("session");
                assert_eq!(session.len(), 2);
                let expires = chrono::DateTime::parse_from_rfc3339(session["expiresAt"].as_str().expect("expiry")).expect("timestamp");
                let rounded = chrono::DateTime::from_timestamp(expires.timestamp() + i64::from(expires.timestamp_subsec_nanos() != 0), 0).expect("rounded expiry");
                let cookie = cookie.as_ref().expect("session cookie").to_str().expect("ASCII cookie");
                assert!(cookie.starts_with(&format!("__Host-nucleus-session-{}=", session["id"].as_str().expect("session id"))));
                let attributes: Vec<_> = cookie.split("; ").skip(1).collect();
                assert_eq!(attributes.len(), 5);
                for expected in ["Secure", "HttpOnly", "SameSite=Lax", "Path=/"] { assert!(attributes.contains(&expected)); }
                assert!(attributes.contains(&format!("Expires={}", rounded.format("%a, %d %b %Y %H:%M:%S GMT")).as_str()));
                assert!(!cookie.contains("Max-Age") && !cookie.contains("Domain="));
                current_http(&base, cookie, session["id"].as_str().expect("session id"), "0010").await;
                let token = cookie.split(';').next().expect("cookie pair").split_once('=').expect("cookie token").1;
                let observer = connections(&server, &name, "postgres").recover().await.expect("isolated session query blocker");
                observer.client().batch_execute("BEGIN; SET LOCAL lock_timeout = '5s'; LOCK TABLE identity.sessions IN ACCESS EXCLUSIVE MODE").await.expect("block session queries");
                for (method, path) in [(reqwest::Method::GET, "/api/v1/accounts/current"), (reqwest::Method::DELETE, "/api/v1/sessions/current"), (reqwest::Method::DELETE, "/api/v1/sessions"), (reqwest::Method::GET, "/api/v1/revocations")] {
                    let response = client.request(method, format!("{base}{path}"))
                        .header("operation-id", OPERATION)
                        .header("authorization", format!("Bearer {token}"))
                        .header("session-id", session["id"].as_str().expect("selected session"))
                        .send().await.expect("mixed proof rejected while session queries are blocked");
                    assert_eq!(response.status(), 400);
                    assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe unsupported proof rejection"), serde_json::json!({"code":"invalid_input"}));
                }
                observer.client().batch_execute("ROLLBACK").await.expect("release session queries");
                observer.terminate().await.expect("session query blocker ended");
                let response = client.get(format!("{base}/api/v1/accounts/current")).header("operation-id", OPERATION).header("authorization", format!("Bearer {token}")).send().await.expect("website token cannot select mini transport");
                assert_eq!(response.status(), 401);
            } else {
                assert!(cookie.is_none());
                if kind == "suspended" {
                    assert_eq!(original, serde_json::json!({"code":"permission_denied"}));
                } else {
                    assert!(original.get("session").is_none());
                    assert_eq!(original["registration"].as_object().expect("registration").len(), 3);
                    assert_eq!(original["registration"]["token"].as_str().expect("token").len(), 43);
                }
            }
            let response = create_session().header("session-transport", "bearer").send().await.expect("extra header cannot change original delivery");
            assert_eq!(response.status(), expected);
            assert_eq!(response.headers().get("set-cookie"), cookie.as_ref());
            assert_eq!(response.json::<serde_json::Value>().await.expect("replay result"), original);
            if kind == "registration" {
                let mut changed = body.clone();
                changed["secret"] = "__________________________________________8".into();
                changed["credential"]["mobile"] = "13800000001".into();
                let response = client.post(format!("{base}/api/v1/sessions")).header("operation-id", OPERATION).header("x-real-ip", "192.0.2.2").json(&changed).send().await.expect("proof checked before input mismatch");
                assert_eq!(response.status(), 401);
                assert_eq!(response.json::<serde_json::Value>().await.expect("proof rejection"), serde_json::json!({"code":"authentication_invalid"}));
                changed["secret"] = body["secret"].clone();
                let response = client.post(format!("{base}/api/v1/sessions")).header("operation-id", OPERATION).header("x-real-ip", "192.0.2.2").json(&changed).send().await.expect("valid proof then mismatched input");
                assert_eq!(response.status(), 409);
                assert_eq!(response.json::<serde_json::Value>().await.expect("input rejection"), serde_json::json!({"code":"request_mismatch"}));
                let registration = serde_json::json!({"id":original["registration"]["id"],"token":original["registration"]["token"],"name":"Fixture Operator","idNumber":"00000020000229007X"});
                let observer = connections(&server, &name, "clerk").recover().await.expect("independent R2 observer");
                for (field, value) in [("id", "invalid"), ("token", "private-invalid-token"), ("name", "  "), ("idNumber", "private-invalid-number"), ("mobile", "13800000000"), ("transport", "bearer")] {
                    let mut invalid = registration.clone(); invalid[field] = value.into();
                    let response = client.post(format!("{base}/api/v1/registrations")).header("operation-id", OTHER).header("x-real-ip", "192.0.2.4").json(&invalid).send().await.expect("invalid R2 field");
                    assert_eq!(response.status(), 422, "{field}");
                    assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe body"), serde_json::json!({"code":"invalid_input"}));
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.attempts", &[]).await.expect("no attempt").get::<_, i64>(0), 0);
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.registrations", &[]).await.expect("credential retained").get::<_, i64>(0), 1);
                }
                let register = |body: serde_json::Value| client.post(format!("{base}/api/v1/registrations")).header("operation-id", OTHER).header("x-real-ip", "192.0.2.5").json(&body);
                let response = register(registration.clone()).header("origin", "https://attacker.invalid").send().await.expect("verified website credential still requires Origin");
                assert_eq!(response.status(), 403);
                let blocker = connections(&server, &name, "postgres").recover().await.expect("credential blocker");
                blocker.client().batch_execute("BEGIN").await.expect("blocker transaction");
                let blocker_pid: i32 = blocker.client().query_one("SELECT pg_backend_pid() FROM identity.registrations WHERE id=$1::text::uuid FOR UPDATE", &[&registration["id"].as_str().expect("registration id")]).await.expect("credential row lock").get(0);
                let first = tokio::spawn(register(registration.clone()).send());
                timeout(Duration::from_secs(2), async {
                    loop {
                        let waiting: bool = observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))", &[&blocker_pid]).await.expect("observe original registration lock wait").get(0);
                        if waiting { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("original registration reached its lock");
                let mut changed = registration.clone(); changed["name"] = "Different Operator".into(); changed["token"] = "__________________________________________8".into();
                let response = timeout(Duration::from_secs(2), register(changed.clone()).send()).await.expect("wrong proof does not wait for original work").expect("in-flight proof response");
                assert_eq!(response.status(), 401); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe proof response"), serde_json::json!({"code":"authentication_invalid"}));
                changed["token"] = registration["token"].clone();
                let response = timeout(Duration::from_secs(2), register(changed).send()).await.expect("different input does not wait for original work").expect("in-flight mismatch response");
                assert_eq!(response.status(), 409); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe mismatch response"), serde_json::json!({"code":"request_mismatch"}));
                assert!(!first.is_finished());
                proxy.arm(fixture::database_proxy::Fault::LostCommit);
                blocker.client().batch_execute("ROLLBACK").await.expect("release original work");
                blocker.terminate().await.expect("blocker ended");
                timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("first R2 business COMMIT response lost");
                proxy.release();
                let response = first.await.expect("R2 first caller").expect("R2 first creation");
                assert_eq!(response.status(), 201);
                let cookie = response.headers().get("set-cookie").cloned().expect("R2 cookie");
                let result = response.json::<serde_json::Value>().await.expect("R2 session");
                assert_eq!(result.as_object().expect("envelope").len(), 2);
                assert_eq!(result["session"].as_object().expect("session").len(), 2);
                let session_id = result["session"]["id"].as_str().expect("session id");
                let paired: bool = observer.client().query_one("SELECT receipt.operation_id = $1::text::uuid AND receipt.id = attempt.id AND receipt.operation_id = attempt.operation_id AND receipt.proof = attempt.proof AND receipt.credential_id = attempt.credential_id AND receipt.person_id = account.person_id AND receipt.revision_id = person.revision_id AND person.sequence = 1 AND account.sequence = 1 AND account.version = 1 AND creation.sequence = 1 AND creation.person_id = receipt.person_id AND creation.revision_id = receipt.revision_id AND creation.source = 'registration' AND creation.operation_id = receipt.operation_id AND creation.created_at = attempt.accepted_at AND session.sequence = 1 AND session.version = 1 AND session.revoked_at IS NULL AND issuance.sequence = 1 AND issuance.operation_id = receipt.operation_id AND issuance.account_id = session.account_id AND issuance.authentication_id = session.authentication_id AND issuance.receipt_id = receipt.id AND issuance.hash = session.hash AND issuance.created_at = session.created_at AND issuance.expires_at = session.expires_at AND holder.sequence = 1 AND appointment.sequence = 1 AND appointment.revision_id = receipt.revision_id AND appointment.operator_id = receipt.revision_id AND appointment.operation_id = receipt.operation_id AND role.scope = 'institution' AND role.level = 1 AND role.name = '普通账户' AND account.created_at = attempt.accepted_at AND appointment.assigned_at = attempt.accepted_at AND session.created_at = attempt.accepted_at AND authentication.status = 'registrable' AND authentication.operation_id = $2::text::uuid AND session.id = $3::text::uuid AND session.expires_at = $4::text::timestamptz AND account.avatar <> account.wechat AND NOT EXISTS (SELECT 1 FROM identity.registrations WHERE id = receipt.credential_id) FROM identity.receipts receipt JOIN identity.attempts attempt ON attempt.id = receipt.id JOIN identity.accounts account ON account.id = receipt.account_id JOIN identity.creations creation ON creation.id = account.id JOIN identity.persons person ON person.id = account.person_id JOIN identity.revisions revision ON revision.id = receipt.revision_id JOIN identity.avatars avatar ON avatar.account_id = account.id AND avatar.released_at IS NULL JOIN access.holders holder ON holder.id = account.id JOIN access.appointments appointment ON appointment.id = holder.assignment_id JOIN access.roles role ON role.id = holder.role_id JOIN identity.sessions session ON session.receipt_id = receipt.id JOIN identity.issuances issuance ON issuance.id = session.id JOIN identity.handoffs handoff ON handoff.id = session.id JOIN identity.authentications authentication ON authentication.id = session.authentication_id", &[&OTHER, &OPERATION, &session_id, &result["session"]["expiresAt"].as_str().expect("expiry")]).await.expect("independent full graph before replay").get(0);
                assert!(paired);
                current_http(&base, cookie.to_str().expect("cookie"), session_id, "007X").await;
                proxy.arm(fixture::database_proxy::Fault::LostReadCommit);
                let replay = tokio::spawn(register(registration.clone()).send());
                timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("original Receipt transaction COMMIT response lost after proof preflight");
                proxy.release();
                let response = replay.await.expect("replay caller").expect("R2 replay");
                assert_eq!(response.status(), 200);
                assert_eq!(response.headers().get("set-cookie"), Some(&cookie));
                assert_eq!(response.json::<serde_json::Value>().await.expect("same original session"), result);
                let mut equivalent = registration.clone(); equivalent["name"] = "  Fixture Operator  ".into(); equivalent["idNumber"] = "00000020000229007x".into();
                let response = register(equivalent).send().await.expect("canonical equivalent replay");
                assert_eq!(response.status(), 200);
                assert_eq!(response.headers().get("set-cookie"), Some(&cookie));
                assert_eq!(response.json::<serde_json::Value>().await.expect("canonical result"), result);
                let mut changed = registration.clone(); changed["name"] = "Different Operator".into(); changed["token"] = "__________________________________________8".into();
                let response = register(changed.clone()).send().await.expect("R2 proof precedes mismatch");
                assert_eq!(response.status(), 401); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("proof response"), serde_json::json!({"code":"authentication_invalid"}));
                changed["token"] = registration["token"].clone();
                let response = register(changed).send().await.expect("R2 mismatch");
                assert_eq!(response.status(), 409); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch response"), serde_json::json!({"code":"request_mismatch"}));
                let response = register(registration.clone()).send().await.expect("invalid replay did not bind the original operation");
                assert_eq!(response.status(), 200);
                assert_eq!(response.headers().get("set-cookie"), Some(&cookie));
                assert_eq!(response.json::<serde_json::Value>().await.expect("same result after invalid replays"), result);
                let response = create_session().send().await.expect("original R1 cannot deliver R2");
                assert_eq!(response.status(), 410); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("R1 consumed credential"), serde_json::json!({"code":"verification_required"}));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.attempts", &[]).await.expect("one committed admission").get::<_, i64>(0), 1);
                observer.terminate().await.expect("R2 observer ended");
            }
            assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert!(timeout(Duration::from_millis(20), sms.request()).await.is_err());
            stop.send(()).expect("stop");
            let (application, result) = task.await.expect("server"); result.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert!(timeout(Duration::from_millis(20), sms.request()).await.is_err());
            drop(supplier); sms.stop().await; proxy.stop().await;
        }
    });
}

#[test]
fn challenge_readback_keeps_http_results_and_one_final_diagnostic() {
    use fixture::database_proxy::{Fault, Proxy};

    let case_budget = Duration::from_secs(35);
    #[derive(Clone, Copy)]
    enum Scenario {
        Sent,
        Rejected,
        MessageFailure,
        Cancelled,
    }

    // Independent Python hashlib of LP(challenge/operation, UUID16), first eight bytes.
    let cases = [
        (
            Scenario::Sent,
            Fault::ReadCommitTermination,
            8571406801630286435_i64,
        ),
        (
            Scenario::Rejected,
            Fault::LostReadCommit,
            8265483834529247205,
        ),
        (
            Scenario::MessageFailure,
            Fault::LostReadCommit,
            1937594695433692143,
        ),
        (
            Scenario::Cancelled,
            Fault::StalledReadCommit,
            -7807715451143668649,
        ),
    ];
    if std::env::var_os("NUCLEUS_TEST_CHALLENGE_READBACK").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let child = tokio::process::Command::new(
                std::env::current_exe().expect("test executable"),
            )
            .args([
                "--exact",
                "authentication::challenge_readback_keeps_http_results_and_one_final_diagnostic",
                "--nocapture",
            ])
            .env("NUCLEUS_TEST_CHALLENGE_READBACK", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("isolated log process");
            let child_budget = case_budget * u32::try_from(cases.len()).expect("scenario count")
                + Duration::from_secs(5);
            let output = timeout(child_budget, child.wait_with_output())
                .await
                .expect("child budget")
                .expect("child output");
            let text = String::from_utf8(output.stderr).expect("safe JSON diagnostics");
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
                4,
                "one final technical event per action: {text}"
            );
            for (index, (scenario, _, _)) in cases.into_iter().enumerate() {
                let operation = format!("00000000-0000-4000-8000-{:012x}", index + 1);
                let matching: Vec<_> = failures
                    .iter()
                    .filter(|event| event["fields"]["operation_id"] == operation)
                    .collect();
                assert_eq!(matching.len(), 1, "single final event for {operation}");
                let event = matching[0];
                let span = event["spans"]
                    .as_array()
                    .expect("spans")
                    .iter()
                    .find(|span| span["route"] == "/api/v1/codes")
                    .expect("original request span");
                let request = span["request_id"].as_str().expect("request identity");
                Uuid::parse_str(request).expect("request UUID");
                let error = event["fields"]["error"]
                    .as_str()
                    .expect("both cleanup and original reason");
                assert!(
                    error.contains("ChallengeReadback")
                        && error.contains("cause: Combined { cause: Query(connection closed)"),
                    "{event}"
                );
                match scenario {
                    Scenario::Sent => {
                        assert_eq!(event["fields"]["stage"], "challenge_completion");
                        assert!(
                            error.contains("commit: Ok(())")
                                && error.contains(
                                    "termination: Err(database driver connection failed)"
                                )
                        );
                    }
                    Scenario::Rejected => {
                        assert_eq!(event["fields"]["stage"], "authentication_completion");
                        assert!(
                            error.starts_with("ChallengeConclusion {")
                                && error.contains("cause: Rejected")
                                && error.contains("commit: Err(Commit(")
                        );
                    }
                    Scenario::MessageFailure => {
                        assert!(
                            error.starts_with("ChallengeConclusion {")
                                && error.contains("NotCommitted")
                                && error.contains("SQLSTATE 42501")
                                && error.contains("commit: Err(Commit(")
                        );
                    }
                    Scenario::Cancelled => {
                        assert_eq!(event["fields"]["stage"], "challenge_completion");
                        assert!(
                            error.contains("commit: Err(CommitTimeout(")
                                && error.contains("termination: Ok(())")
                        );
                    }
                }
                if !matches!(scenario, Scenario::Cancelled) {
                    let response = events
                        .iter()
                        .find(|event| {
                            event["fields"]["stage"] == "http_response"
                                && event["spans"].as_array().is_some_and(|spans| {
                                    spans.iter().any(|span| span["request_id"] == request)
                                })
                        })
                        .expect("same request HTTP response");
                    assert_eq!(
                        response["fields"]["status"],
                        match scenario {
                            Scenario::Rejected => 403,
                            Scenario::MessageFailure => 503,
                            _ => 200,
                        }
                    );
                }
            }
            for secret in [
                "13800000000",
                "fictional-token",
                "fictional-secret",
                "FictionalAccessKey",
                "ACS3-HMAC-SHA256",
                "Authorization: ",
                "fictional-sensitive-termination-detail",
                fixture::PASSWORD,
                "SELECT ",
                "INSERT ",
            ] {
                assert!(
                    !events.iter().any(|event| contains_text(event, secret)),
                    "secret in diagnostics"
                );
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("actual driver scheduling").block_on(async {
            for (index,(scenario,fault,lock)) in cases.into_iter().enumerate() {
                timeout(case_budget,async {
                    let name=database(&server).await;
                    let operation=format!("00000000-0000-4000-8000-{:012x}",index+1);
                    let supplier=if matches!(scenario,Scenario::Rejected) {Supplier::new().await} else {Supplier::responding(serde_json::json!({"success":true,"challenge_ts":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros,true),"hostname":"example.com","action":"send_authentication_code","cdata":operation}).to_string()).await};
                    let mut sms=fixture::supplier::Supplier::start().await; let proxy=Proxy::start(&server).await;
                    let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(4).expect("settings"),process::Settings::new(Duration::from_secs(20)).expect("stop budget"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("application");
                    let original=application.authentication().verify(input(&operation,"13800000000","fictional-token")).await.expect("durable original terminal");
                    let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
                    let snapshot="SELECT to_jsonb(challenge)::text FROM identity.challenges challenge";
                    let before:String=observer.client().query_one(snapshot,&[]).await.expect("original graph").get(0);
                    let owner=connections(&server,&name,"postgres").recover().await.expect("isolated permission fixture");
                    if matches!(scenario,Scenario::MessageFailure) {
                        owner.client().batch_execute("REVOKE INSERT (id, challenge_id, ordinal, worker_id, created_at, expires_at) ON identity.messages FROM clerk").await.expect("remove actual message insertion columns");
                        assert!(!owner.client().query_one("SELECT has_any_column_privilege('clerk','identity.messages','INSERT')",&[]).await.expect("effective column privilege").get::<_,bool>(0));
                    }
                    let gate=connections(&server,&name,"clerk").recover().await.expect("original operation gate");
                    gate.client().batch_execute("BEGIN").await.expect("begin gate");
                    gate.client().query_one("SELECT pg_advisory_xact_lock($1)",&[&lock]).await.expect("exact operation gate");
                    let pid:i32=gate.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("gate pid").get(0);
                    let (base,stop,mut task)=running(application).await;
                    // The empty first retention round must finish its Clock transactions before arming BEGIN.
                    timeout(Duration::from_secs(3),async {loop {
                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND usename='clerk' AND state='idle' AND query LIKE 'SELECT family, operation_id, action, target, input, subject, person_id, revision_id, created_at, expires_at, seal, seal_key_id, seal_version FROM workforce.completions WHERE expires_at <= %')",&[]).await.expect("last empty retention scan").get::<_,bool>(0) {break;}
                        tokio::task::yield_now().await;
                    }}).await.expect("initial retention round completed");
                    let request=||http_client().post(format!("{base}/api/v1/codes")).header("operation-id",&operation).json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"})).timeout(Duration::from_secs(25)).send();
                    proxy.arm(Fault::RejectedBegin);
                    let caller=tokio::spawn(request());
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("original online transaction fails");
                    let recovery_pid=timeout(Duration::from_secs(3),async {loop {
                        if let Some(row)=observer.client().query_opt("SELECT waiting.pid FROM pg_locks waiting JOIN pg_locks held USING(locktype,database,classid,objid,objsubid) WHERE waiting.locktype='advisory' AND NOT waiting.granted AND waiting.mode='ExclusiveLock' AND held.granted AND held.pid=$1 AND held.mode='ExclusiveLock' AND waiting.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND $1=ANY(pg_blocking_pids(waiting.pid))",&[&pid]).await.expect("recovery at exact held operation") {break row.get::<_,i32>(0);}
                        tokio::task::yield_now().await;
                    }}).await.expect("recovery lock wait");
                    proxy.arm(fault);gate.client().batch_execute("COMMIT").await.expect("release gate");gate.terminate().await.expect("gate ended");
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("readback cleanup fault");
                    if matches!(scenario,Scenario::Cancelled) {
                        caller.abort();assert!(caller.await.expect_err("cancelled caller").is_cancelled());
                        stop.send(()).expect("stop while actual work remains");
                        assert!(timeout(Duration::from_millis(30),&mut task).await.is_err(),"stop keeps accepted action");
                        accept_sms(timeout(Duration::from_secs(12),sms.request()).await.expect("message after readback timeout"));
                        let (application,result)=task.await.expect("server drain");result.expect("accepted work drains");drop(application);
                    } else {
                        if matches!(scenario,Scenario::Sent) {accept_sms(sms.request().await);}
                        let response=caller.await.expect("caller").expect("HTTP result");
                        assert_eq!(response.status(),match scenario {Scenario::Rejected=>403,Scenario::MessageFailure=>503,_=>200});
                        let body=response.json::<serde_json::Value>().await.expect("response body");
                        assert_eq!(body["code"],match scenario {Scenario::Rejected=>"verification_invalid",Scenario::MessageFailure=>"dependency_unavailable",_=>"ok"});
                        stop.send(()).expect("stop");let (application,result)=task.await.expect("server drain");result.expect("drained");drop(application);
                    }
                    // Observe durable effects before another service can recover or replay them.
                    let after:String=observer.client().query_one(snapshot,&[]).await.expect("original challenge before replay").get(0); assert_eq!(before,after);
                    let sent=matches!(scenario,Scenario::Sent|Scenario::Cancelled);
                    let rows=observer.client().query("SELECT message.id,message.status,check_row.id AS check_id,claim.id AS claim_id FROM identity.messages message JOIN identity.checks check_row ON check_row.id=message.id JOIN identity.claims claim ON claim.id=message.id WHERE message.challenge_id=$1",&[&original.id()]).await.expect("original message graph before replay");
                    assert_eq!(rows.len(),usize::from(sent));
                    if let Some(row)=rows.first() {assert_eq!(row.get::<_,&str>("status"),"sent");assert_eq!(row.get::<_,Uuid>("id"),row.get::<_,Uuid>("check_id"));assert_eq!(row.get::<_,Uuid>("id"),row.get::<_,Uuid>("claim_id"));}
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.messages",&[]).await.expect("no partial message").get::<_,i64>(0),i64::from(sent));
                    proxy.release();
                    timeout(Duration::from_secs(3),async {loop {if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",&[&recovery_pid]).await.expect("backend end").get::<_,bool>(0){break;}tokio::task::yield_now().await;}}).await.expect("actual readback backend ends");
                    if matches!(scenario,Scenario::MessageFailure) {owner.client().batch_execute("GRANT INSERT (id, challenge_id, ordinal, worker_id, created_at, expires_at) ON identity.messages TO clerk").await.expect("restore fixture permission");}
                    let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(4).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("restart");
                    assert_eq!(application.authentication().verify(input(&operation,"13800000000","fictional-token")).await.expect("original challenge replay"),original);
                    let (base,stop,task)=running(application).await;
                    if sent {
                        let response=http_client().post(format!("{base}/api/v1/codes")).header("operation-id",&operation).json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"})).send().await.expect("original message replay");assert_eq!(response.status(),200);
                        assert_eq!(response.json::<serde_json::Value>().await.expect("original message")["message"]["id"],rows[0].get::<_,Uuid>("id").to_string());
                    }
                    stop.send(()).expect("stop replay");let (application,result)=task.await.expect("replay drain");result.expect("drained");drop(application);
                    assert_eq!(supplier.calls.load(Ordering::SeqCst),1);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no extra PNVS through replay and drain");
                    observer.terminate().await.expect("observer ended");owner.terminate().await.expect("owner ended");sms.stop().await;proxy.stop().await;
                }).await.unwrap_or_else(|error| panic!("HTTP challenge case {index} deadline: {error}"));
            }
        });
    });
}

#[test]
fn message_preparation_keeps_both_cleanup_causes_and_the_supplier_failure() {
    use fixture::database_proxy::{Fault, Proxy};

    if std::env::var_os("NUCLEUS_TEST_MESSAGE_READBACK").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let expected = tempfile::NamedTempFile::new().expect("private expected codes");
            let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact","authentication::message_preparation_keeps_both_cleanup_causes_and_the_supplier_failure","--nocapture"])
                .env("NUCLEUS_TEST_MESSAGE_READBACK","1").env("NUCLEUS_TEST_MESSAGE_EXPECTATIONS",expected.path())
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().expect("isolated diagnostics child");
            let output = timeout(Duration::from_secs(40),child.wait_with_output()).await.expect("child deadline").expect("child output");
            let text = String::from_utf8(output.stderr).expect("JSON diagnostics"); assert!(output.status.success(),"{text}");
            let events: Vec<serde_json::Value> = text.lines().map(|line|serde_json::from_str(line).expect("JSON event")).collect();
            let errors: Vec<_> = events.iter().filter(|event|event["level"]=="ERROR").collect();
            assert_eq!(errors.len(),2,"one final event per accepted action: {text}");
            for (index,cancelled) in [false,true].into_iter().enumerate() {
                let operation = format!("00000000-0000-4000-8000-{:012x}",index+1);
                let matching: Vec<_> = errors.iter().filter(|event|event["fields"]["operation_id"]==operation).collect(); assert_eq!(matching.len(),1);
                let event = matching[0];
                let error = event["fields"]["error"].as_str().expect("three actual causes");
                if cancelled {
                    assert_eq!(event["fields"]["stage"],"message_completion");assert_eq!(event["fields"]["outcome"],"sent");
                    assert!(error.starts_with("MessagePreparation { cause: MessageReadback { cause: Combined { cause: Query(connection closed)"));
                    assert!(error.contains("commit: Err(CommitTimeout(") && error.contains("termination: Ok(())"),"{event}");
                    assert!(error.contains("preparation: MessageReadback { cause: Combined { cause: Query(connection closed)"),"{event}");
                    assert!(error.contains("commit: Ok(()), termination: Err(database driver connection failed)"),"{event}");
                    assert!(event["fields"]["challenge"].as_str().expect("Challenge cleanup").starts_with("Some(Combined { cause: Query(connection closed)"));
                } else {
                    assert_eq!(event["fields"]["stage"],"authentication_completion"); assert_eq!(event["fields"]["kind"],"unavailable");
                    assert!(error.starts_with("ChallengeConclusion { cause: MessagePreparation { cause: MessageTransition { cause: MessageOutcome { reason: HttpStatus, status: 503, elapsed:"),"{event}");
                    assert!(error.contains("transition: MessageReadback { cause: Combined { cause: Query(connection closed)"),"{event}");
                    assert!(error.contains("commit: Err(Commit(connection closed))"),"{event}");
                    assert!(error.contains("preparation: MessageReadback { cause: Combined { cause: Query(connection closed)"),"{event}");
                    assert!(error.contains("challenge: Combined { cause: Query(connection closed)"),"{event}");
                    assert!(error.contains("commit: Ok(()), termination: Err(database driver connection failed)"),"{event}");
                }
                let span = event["spans"].as_array().expect("spans").iter().find(|span|span["route"]=="/api/v1/codes").expect("original request span");
                let request = span["request_id"].as_str().expect("request ID"); Uuid::parse_str(request).expect("request UUID");
                if !cancelled {
                    let response = events.iter().find(|event|event["fields"]["stage"]=="http_response" && event["spans"].as_array().is_some_and(|spans|spans.iter().any(|span|span["request_id"]==request))).expect("same request response");
                    assert_eq!(response["fields"]["status"],503);
                }
            }
            let codes: Vec<String> = serde_json::from_slice(&std::fs::read(expected.path()).expect("private codes")).expect("expected codes");
            for secret in codes.iter().map(String::as_str).chain(["13800000000","fictional-token","fictional-secret","FictionalAccessKey","ACS3-HMAC-SHA256","Authorization: ","fictional-sensitive-termination-detail",fixture::PASSWORD,"SELECT ","INSERT "]) {
                assert!(!events.iter().any(|event|contains_text(event,secret)),"private material in diagnostic strings");
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("actual driver scheduling").block_on(async {
            timeout(Duration::from_secs(35),async {
                let mut codes = Vec::new();
                // Independent SHA-256 prefix of LP(challenge/operation, UUID16).
                for (index,(cancelled,lock)) in [(false,8571406801630286435_i64),(true,8265483834529247205)].into_iter().enumerate() {
                    let name=database(&server).await;
                    let operation=format!("00000000-0000-4000-8000-{:012x}",index+1);
                    let supplier=Supplier::responding(serde_json::json!({"success":true,"challenge_ts":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros,true),"hostname":"example.com","action":"send_authentication_code","cdata":operation}).to_string()).await;
                    let mut sms=fixture::supplier::Supplier::start().await;let proxy=Proxy::start(&server).await;
                    let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(4).expect("settings"),process::Settings::new(Duration::from_secs(20)).expect("stop budget"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("application");
                    let original=application.authentication().verify(input(&operation,"13800000000","fictional-token")).await.expect("durable Passed");
                    let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
                    let snapshot="SELECT to_jsonb(challenge)::text FROM identity.challenges challenge";
                    let before:String=observer.client().query_one(snapshot,&[]).await.expect("original Challenge").get(0);
                    let (base,stop,mut task)=running(application).await;
                    timeout(Duration::from_secs(3),async {loop {
                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND usename='clerk' AND state='idle' AND query LIKE 'SELECT family, operation_id, action, target, input, subject, person_id, revision_id, created_at, expires_at, seal, seal_key_id, seal_version FROM workforce.completions WHERE expires_at <= %')",&[]).await.expect("empty initial retention round").get::<_,bool>(0) {break;}
                        tokio::task::yield_now().await;
                    }}).await.expect("initial retention sampling completed");
                    let waiting = async || {
                        timeout(Duration::from_secs(3),async {loop {
                            if let Some(row)=observer.client().query_opt("SELECT waiting.pid,held.pid FROM pg_locks waiting JOIN pg_locks held USING(locktype,database,classid,objid,objsubid) WHERE waiting.locktype='advisory' AND NOT waiting.granted AND held.granted AND waiting.mode='ExclusiveLock' AND held.mode='ExclusiveLock' AND waiting.classid=(($1::bigint >> 32) & 4294967295)::oid AND waiting.objid=($1::bigint & 4294967295)::oid AND waiting.objsubid=1 AND waiting.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND held.pid=ANY(pg_blocking_pids(waiting.pid))",&[&lock]).await.expect("exact original operation wait") {break (row.get::<_,i32>(0),row.get::<_,i32>(1));}
                            tokio::task::yield_now().await;
                        }}).await.expect("actual original lock protects next stage")
                    };
                    proxy.arm(Fault::DelayedReadCommit);
                    let caller=tokio::spawn(http_client().post(format!("{base}/api/v1/codes")).header("operation-id",&operation).json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"})).timeout(Duration::from_secs(25)).send());
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("Challenge read COMMIT held before forwarding");
                    let (_,challenge_pid)=waiting().await;
                    assert_eq!(observer.client().query_one("SELECT (SELECT count(*) FROM identity.messages)+(SELECT count(*) FROM identity.checks)+(SELECT count(*) FROM identity.claims)",&[]).await.expect("no early preparation").get::<_,i64>(0),0);
                    assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no PNVS before old Challenge transaction ends");
                    proxy.arm(Fault::DelayedCommit);proxy.release();
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("new permit COMMIT held");
                    let (recovery_pid,permit_pid)=waiting().await;
                    assert_eq!(observer.client().query_one("SELECT (SELECT count(*) FROM identity.messages)+(SELECT count(*) FROM identity.checks)+(SELECT count(*) FROM identity.claims)",&[]).await.expect("uncommitted original permit").get::<_,i64>(0),0);
                    assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err());
                    proxy.arm(Fault::ReadCommitTermination);proxy.release();
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("Message readback cleanup");
                    let request=timeout(Duration::from_secs(12),sms.request()).await.expect("exactly one PNVS after complete preparation and claim");
                    let wire=std::str::from_utf8(&request.bytes).expect("request bytes");let target=wire.lines().next().expect("request line").split(' ').nth(1).expect("target");
                    let url=url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("supplier URL");
                    let template=url.query_pairs().find(|(key,_)|key=="TemplateParam").expect("template").1.into_owned();
                    let code=serde_json::from_str::<serde_json::Value>(&template).expect("template JSON")["code"].as_str().expect("code").to_owned();
                    let sql="SELECT message.id,message.worker_id,message.status,message.finished_at,(to_jsonb(message)-'status'-'finished_at')::text AS permit,to_jsonb(check_row)::text AS proof,to_jsonb(claim)::text AS claim,check_row.key_id,check_row.hash,claim.worker_id AS claimed_worker FROM identity.messages message JOIN identity.checks check_row ON check_row.id=message.id JOIN identity.claims claim ON claim.id=message.id WHERE message.challenge_id=$1";
                    let claimed=observer.client().query_one(sql,&[&original.id()]).await.expect("independent confirmed claim before supplier reply");
                    let id:Uuid=claimed.get("id");
                    assert_eq!(url.query_pairs().find(|(key,_)|key=="OutId").expect("OutId").1,id.to_string());
                    assert!(message_proof_matches(&claimed,&code),"original proof matches request code");
                    assert_eq!(claimed.get::<_,Uuid>("worker_id"),claimed.get::<_,Uuid>("claimed_worker"));assert_eq!(claimed.get::<_,&str>("status"),"sending");assert!(claimed.get::<_,Option<chrono::DateTime<chrono::Utc>>>("finished_at").is_none());
                    codes.push(code);
                    proxy.arm(Fault::DelayedCommit);
                    if cancelled { accept_sms(request); }
                    else { request.reply.send(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).expect("supplier 503"); }
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("supplier conclusion COMMIT held");
                    let (conclusion_recovery_pid,conclusion_pid)=waiting().await;
                    assert_eq!(observer.client().query_one(sql,&[&original.id()]).await.expect("original sending before conclusion commit").get::<_,&str>("status"),"sending");
                    proxy.arm(if cancelled { Fault::StalledReadCommit } else { Fault::LostReadCommit });proxy.release();
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("complete conclusion readback reached cleanup");
                    let option = if cancelled {
                        caller.abort();assert!(caller.await.expect_err("cancelled HTTP waiter").is_cancelled());
                        stop.send(()).expect("stop retains accepted sender");
                        assert!(timeout(Duration::from_millis(30),&mut task).await.is_err(),"stop cannot release live conclusion cleanup");
                        None
                    } else { Some((caller,stop)) };
                    if let Some((caller,stop))=option {
                        let response=caller.await.expect("caller").expect("HTTP failure response");assert_eq!(response.status(),503);assert_eq!(response.json::<serde_json::Value>().await.expect("failure body")["code"],"dependency_unavailable");stop.send(()).expect("stop");
                    }
                    let (application,result)=task.await.expect("actual drain");result.expect("accepted work drained");drop(application);
                    let after=observer.client().query_one(sql,&[&original.id()]).await.expect("supplier conclusion persisted before replay");
                    let status = if cancelled {"sent"} else {"unknown"};
                    assert_eq!(after.get::<_,&str>("status"),status);assert!(after.get::<_,Option<chrono::DateTime<chrono::Utc>>>("finished_at").is_some());
                    for column in ["permit","proof","claim"] {assert_eq!(after.get::<_,&str>(column),claimed.get::<_,&str>(column));}
                    assert_eq!(observer.client().query_one(snapshot,&[]).await.expect("same Challenge before replay").get::<_,String>(0),before);
                    proxy.release();
                    timeout(Duration::from_secs(3),async {loop {
                        if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=ANY($1))",&[&vec![challenge_pid,permit_pid,recovery_pid,conclusion_pid,conclusion_recovery_pid]]).await.expect("actual old backend termination").get::<_,bool>(0) {break;}
                        tokio::task::yield_now().await;
                    }}).await.expect("all old transaction backends ended");
                    let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(4).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop budget"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("restart");
                    let (base,stop,task)=running(application).await;
                    let response=http_client().post(format!("{base}/api/v1/codes")).header("operation-id",&operation).json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"})).send().await.expect("original Message replay");assert_eq!(response.status(),200);let body=response.json::<serde_json::Value>().await.expect("original response");assert_eq!(body["message"]["id"],id.to_string());assert_eq!(body["message"]["delivery"],status);
                    stop.send(()).expect("stop replay");let (application,result)=task.await.expect("replay drain");result.expect("replay drained");drop(application);
                    assert_eq!(supplier.calls.load(Ordering::SeqCst),1);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"zero extra PNVS through replay and drain");
                    observer.terminate().await.expect("observer ended");sms.stop().await;proxy.stop().await;
                }
                std::fs::write(std::env::var_os("NUCLEUS_TEST_MESSAGE_EXPECTATIONS").expect("private expectation path"),serde_json::to_vec(&codes).expect("expectation JSON")).expect("private expected codes");
            }).await.expect("Message preparation HTTP cases bounded");
        });
    });
}

#[test]
fn session_readback_preserves_http_outcomes_and_single_safe_diagnostics() {
    use fixture::database_proxy::{Fault, Proxy};

    #[derive(Clone, Copy)]
    enum Scenario {
        Session,
        Registration,
        Invalid,
        Suspended,
        Cancelled,
        Refused,
        RefusedCleanup,
    }

    // Independent Python hashlib of LP(authentication/operation, UUID16), first eight bytes.
    let cases = [
        (
            Scenario::Session,
            Fault::ReadCommitTermination,
            -756587184824745026_i64,
        ),
        (
            Scenario::Registration,
            Fault::LostReadCommit,
            2165761717968986713,
        ),
        (
            Scenario::Invalid,
            Fault::LostReadCommit,
            -1881606194887669528,
        ),
        (
            Scenario::Suspended,
            Fault::StalledReadCommit,
            2993173412282227400,
        ),
        (
            Scenario::Cancelled,
            Fault::StalledReadCommit,
            5438930344623448817,
        ),
        (
            Scenario::Refused,
            Fault::RejectedBegin,
            -5467082076798197129,
        ),
        (
            Scenario::RefusedCleanup,
            Fault::RejectedRollback,
            850217005070547604,
        ),
    ];
    if std::env::var_os("NUCLEUS_TEST_SESSION_READBACK").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "authentication::session_readback_preserves_http_outcomes_and_single_safe_diagnostics", "--nocapture"])
                .env("NUCLEUS_TEST_SESSION_READBACK", "1").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().expect("isolated diagnostics process");
            let output = timeout(Duration::from_secs(90), child.wait_with_output()).await.expect("child budget").expect("child result");
            let text = String::from_utf8(output.stderr).expect("JSON diagnostics"); assert!(output.status.success(), "{text}");
            let events: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).expect("JSON event")).collect();
            let failures: Vec<_> = events.iter().filter(|event| event["level"]=="ERROR").collect();
            assert_eq!(failures.len(), 6, "exactly one technical event per affected operation: {text}");
            for (index, (scenario, fault, _)) in cases.into_iter().enumerate() {
                let operation = format!("00000000-0000-4000-8000-{:012x}", index+1);
                let matching: Vec<_> = failures.iter().filter(|event| event["fields"]["operation_id"]==operation).collect();
                if matches!(scenario, Scenario::Refused) { assert!(matching.is_empty(), "ordinary proof refusal is not a technical failure"); continue; }
                assert_eq!(matching.len(), 1, "one final diagnostic: {operation}");
                let event = matching[0]; assert_eq!(event["fields"]["stage"], "authentication_completion");
                let span = event["spans"].as_array().expect("spans").iter().find(|span| span["route"]=="/api/v1/sessions").expect("original HTTP span");
                let request_id = span["request_id"].as_str().expect("request identity"); Uuid::parse_str(request_id).expect("request UUID");
                let cause = event["fields"]["error"].as_str().expect("original failure"); assert!(cause.contains("Query("), "{event}");
                if matches!(scenario, Scenario::RefusedCleanup) {
                    assert_eq!(event["fields"]["outcome"], "known_refusal");
                    assert!(cause.starts_with("SessionRefused {") && cause.contains("refusal:") && cause.contains("Rollback { cause: Proof") && cause.contains("rollback: Rollback(connection closed)"), "{event}");
                } else {
                    assert_eq!(event["fields"]["outcome"], "committed");
                    let commit = event["fields"]["commit"].as_str().expect("commit result");
                    let termination = event["fields"]["termination"].as_str().expect("driver result");
                    match fault {
                        Fault::ReadCommitTermination => { assert_eq!(commit,"Ok(())"); assert_eq!(termination,"Err(database driver connection failed)"); }
                        Fault::StalledReadCommit => { assert!(commit.starts_with("Err(CommitTimeout(")); assert_eq!(termination,"Ok(())"); }
                        _ => assert!(commit.starts_with("Err(Commit(")),
                    }
                }
                if !matches!(scenario, Scenario::Cancelled) {
                    let response = events.iter().find(|event| event["fields"]["stage"]=="http_response" && event["spans"].as_array().is_some_and(|spans| spans.iter().any(|span| span["request_id"]==request_id))).expect("response associated with the same request");
                    assert_eq!(response["fields"]["status"], match scenario { Scenario::Invalid | Scenario::Suspended => 403, Scenario::RefusedCleanup => 503, _ => 200 });
                }
            }
            for secret in ["13800000000", "13900000000", "__Host-nucleus-session", "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8", "__________________________________________8", "fictional-sensitive-termination-detail", fixture::PASSWORD, "SELECT ", "INSERT "] {
                assert!(!events.iter().any(|event| contains_text(event, secret)), "private material in diagnostics");
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("real driver observation").block_on(async {
            timeout(Duration::from_secs(80), async {
                for (index, (scenario, fault, lock)) in cases.into_iter().enumerate() {
                    let name = empty_database(&server).await;
                    initialize(&server, &name, if matches!(scenario, Scenario::Session) { "13800000000" } else { "13900000000" }).await;
                    if matches!(scenario, Scenario::Suspended) {
                        suspended_account(&server, &name).await;
                    }
                    let supplier=Supplier::echoing().await; let mut sms=fixture::supplier::Supplier::start().await; let proxy=Proxy::start(&server).await;
                    let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(15)).expect("stop budget"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("application");
                    let (base,stop,mut task)=running(application).await; let client=http_client();
                    let c1=Uuid::new_v4().to_string();
                    let caller=tokio::spawn(client.post(format!("{base}/api/v1/codes")).header("operation-id",&c1).json(&serde_json::json!({"mobile":"13800000000","token":c1})).send());
                    let sent=timeout(Duration::from_secs(3),sms.request()).await.expect("actual C1 SMS");
                    let wire=std::str::from_utf8(&sent.bytes).expect("SMS request"); let target=wire.lines().next().expect("request line").split(' ').nth(1).expect("target");
                    let url=url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("supplier URL"); let template=url.query_pairs().find(|(key,_)|key=="TemplateParam").expect("template").1.into_owned();
                    let code=serde_json::from_str::<serde_json::Value>(&template).expect("template JSON")["code"].as_str().expect("code").to_owned(); accept_sms(sent);
                    let response=caller.await.expect("C1 caller").expect("C1 response"); assert_eq!(response.status(),200); let message=response.json::<serde_json::Value>().await.expect("message");
                    let operation=format!("00000000-0000-4000-8000-{:012x}",index+1);
                    let body=serde_json::json!({"secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","credential":{"type":"message","id":message["message"]["id"],"mobile":"13800000000","code":if matches!(scenario,Scenario::Invalid) {if code=="000000" {"000001"} else {"000000"}} else {&code}}});
                    let request=|body:&serde_json::Value| client.post(format!("{base}/api/v1/sessions")).header("operation-id",&operation).json(body).timeout(Duration::from_secs(25));
                    let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
                    let refusal=matches!(scenario,Scenario::Refused|Scenario::RefusedCleanup);
                    let mut submitted=body.clone();
                    if refusal {
                        assert_eq!(request(&body).send().await.expect("original R1").status(),200);
                        submitted["secret"]=serde_json::json!("__________________________________________8");
                    }
                    let gate=if refusal {
                        let gate=connections(&server,&name,"clerk").recover().await.expect("original operation gate");
                        gate.client().batch_execute("BEGIN").await.expect("gate transaction"); gate.client().query_one("SELECT pg_catalog.pg_advisory_xact_lock($1)",&[&lock]).await.expect("exact operation gate"); Some(gate)
                    } else {None};
                    proxy.arm(if refusal {Fault::RejectedBegin} else {Fault::DelayedCommit});
                    let mut caller=tokio::spawn(request(&submitted).send());
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("original failure intercepted");
                    timeout(Duration::from_secs(3),async {loop {
                        let waiting:bool=observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND mode='ExclusiveLock' AND objsubid=1 AND ((classid::bigint<<32)|objid::bigint)=$1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND cardinality(pg_catalog.pg_blocking_pids(pid))>0)",&[&lock]).await.expect("exact operation recovery wait").get(0);
                        if waiting {break;} tokio::task::yield_now().await;
                    }}).await.expect("recovery waits before second fault");
                    if !matches!(scenario,Scenario::Refused) {proxy.arm(fault);}
                    if let Some(gate)=gate {gate.client().batch_execute("ROLLBACK").await.expect("release gate");gate.terminate().await.expect("gate ended");} else {proxy.release();}
                    if !matches!(scenario,Scenario::Refused) {timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("readback cleanup fault");}
                    let row=observer.client().query_one("SELECT authentication.status, authentication.message_id=$2::text::uuid AND authentication.proof=decode('630dcd2966c4336691125448bbb25b4ff412a49c732db2c8abc1b8581bd710dd','hex') AS paired, check_record.failures, check_record.authentication_id=authentication.id AS consumed, session.id IS NOT NULL AND handoff.id=session.id AND session.created_at=authentication.finished_at AS session, registration.id IS NOT NULL AND registration.created_at=authentication.finished_at AS registration FROM identity.authentications authentication JOIN identity.checks check_record ON check_record.id=authentication.message_id LEFT JOIN identity.sessions session ON session.authentication_id=authentication.id LEFT JOIN identity.handoffs handoff ON handoff.id=session.id LEFT JOIN identity.registrations registration ON registration.authentication_id=authentication.id WHERE authentication.operation_id=$1::text::uuid",&[&operation,&message["message"]["id"].as_str().expect("message ID")]).await.expect("independent original facts before replay");
                    assert!(row.get::<_,bool>("paired"));
                    assert_eq!(row.get::<_,&str>("status"),match scenario {Scenario::Session=>"authenticated",Scenario::Invalid=>"invalid",Scenario::Suspended=>"suspended",_=>"registrable"});
                    assert_eq!(row.get::<_,i16>("failures"),i16::from(matches!(scenario,Scenario::Invalid)));
                    if !matches!(scenario,Scenario::Invalid) {assert!(row.get::<_,bool>("consumed"));}
                    if matches!(scenario,Scenario::Session) {assert!(row.get::<_,bool>("session"));}
                    if !matches!(scenario,Scenario::Session|Scenario::Invalid|Scenario::Suspended) {assert!(row.get::<_,bool>("registration"));}
                    let snapshot="SELECT jsonb_build_object('authentications',(SELECT jsonb_agg(to_jsonb(authentication) ORDER BY id) FROM identity.authentications authentication),'checks',(SELECT jsonb_agg(to_jsonb(check_record) ORDER BY id) FROM identity.checks check_record),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session),'handoffs',(SELECT jsonb_agg(to_jsonb(handoff) ORDER BY id) FROM identity.handoffs handoff),'registrations',(SELECT jsonb_agg(to_jsonb(registration) ORDER BY id) FROM identity.registrations registration))::text";
                    let before:String=observer.client().query_one(snapshot,&[]).await.expect("immutable original graph").get(0);
                    let original=if matches!(scenario,Scenario::Cancelled) {
                        caller.abort();assert!(caller.await.expect_err("caller cancelled").is_cancelled());stop.send(()).expect("stop while readback owns work");assert!(timeout(Duration::from_millis(30),&mut task).await.is_err(),"stop waits for actual work");None
                    } else {
                        let response=timeout(Duration::from_secs(13),&mut caller).await.expect("bounded response").expect("caller").expect("HTTP response");
                        assert_eq!(response.status(),match scenario {Scenario::Invalid|Scenario::Suspended=>403,Scenario::Refused=>401,Scenario::RefusedCleanup=>503,_=>200});
                        let cookie=response.headers().get("set-cookie").map(|value|value.to_str().expect("cookie").to_owned());assert_eq!(cookie.is_some(),matches!(scenario,Scenario::Session));
                        let json=response.json::<serde_json::Value>().await.expect("response JSON");
                        match scenario {Scenario::Invalid=>assert_eq!(json,serde_json::json!({"code":"verification_invalid"})),Scenario::Suspended=>assert_eq!(json,serde_json::json!({"code":"permission_denied"})),Scenario::Refused=>assert_eq!(json,serde_json::json!({"code":"authentication_invalid"})),Scenario::RefusedCleanup=>assert_eq!(json,serde_json::json!({"code":"dependency_unavailable"})),_=>assert_eq!(json["code"],"ok")};
                        stop.send(()).expect("stop after result");Some((cookie,json))
                    };
                    let (application,result)=timeout(Duration::from_secs(15),task).await.expect("bounded drain").expect("server task");result.expect("drained");drop(application);proxy.release();
                    timeout(Duration::from_secs(3),async {loop {
                        let ended:bool=observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename='clerk' AND pid<>pg_backend_pid())",&[]).await.expect("actual backends ended").get(0);if ended {break;}tokio::task::yield_now().await;
                    }}).await.expect("old backends ended before restart");
                    let application=Application::start(connections(&server,&name,"clerk"),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop budget"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("restart");
                    let (base,stop,task)=running(application).await;
                    let response=client.post(format!("{base}/api/v1/sessions")).header("operation-id",&operation).json(&body).send().await.expect("original replay");
                    assert_eq!(response.status(),if matches!(scenario,Scenario::Invalid|Scenario::Suspended) {403}else{200});
                    let cookie=response.headers().get("set-cookie").map(|value|value.to_str().expect("cookie").to_owned());let json=response.json::<serde_json::Value>().await.expect("replay JSON");
                    if !refusal && let Some(original)=original {assert_eq!((cookie,json),original,"same original credential and expiry");}
                    assert_eq!(observer.client().query_one(snapshot,&[]).await.expect("graph after replay").get::<_,String>(0),before,"replay cannot modify counters, credentials or deadlines");
                    stop.send(()).expect("stop replay");let (application,result)=task.await.expect("replay task");result.expect("drained");drop(application);
                    assert_eq!(supplier.calls.load(Ordering::SeqCst),1);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no extra call through replay and drain");observer.terminate().await.expect("observer ended");sms.stop().await;proxy.stop().await;
                }
            }).await.expect("HTTP readback acceptance budget");
        });
    });
}

#[test]
fn registration_readback_keeps_http_results_and_one_safe_cleanup_diagnostic() {
    use fixture::database_proxy::{Fault, Proxy};

    #[derive(Clone, Copy)]
    enum Scenario {
        First,
        Replay,
        Cancelled,
        Occupied,
        Revoked,
    }

    let cases = [
        (Scenario::First, Fault::LostReadCommit),
        (Scenario::First, Fault::ReadCommitTermination),
        (Scenario::Cancelled, Fault::StalledReadCommit),
        (Scenario::Replay, Fault::StalledReadCommit),
        (Scenario::Occupied, Fault::LostReadCommit),
        (Scenario::Revoked, Fault::LostReadCommit),
    ];

    if std::env::var_os("NUCLEUS_TEST_REGISTRATION_READBACK").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "authentication::registration_readback_keeps_http_results_and_one_safe_cleanup_diagnostic", "--nocapture"])
                .env("NUCLEUS_TEST_REGISTRATION_READBACK", "1")
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true)
                .spawn().expect("isolated diagnostics process");
            let output = timeout(Duration::from_secs(100), child.wait_with_output()).await.expect("child budget").expect("child output");
            let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
            assert!(output.status.success(), "{text}");
            let events: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).expect("JSON event")).collect();
            let failures: Vec<_> = events.iter().filter(|event| event["level"] == "ERROR").collect();
            assert_eq!(failures.len(), cases.len(), "one final diagnostic per recovered operation: {text}");
            for (index, (scenario, fault)) in cases.into_iter().enumerate() {
                let operation = format!("00000000-0000-4000-8000-{:012x}", index + 1);
                let matching: Vec<_> = failures.iter().filter(|event| event["fields"]["operation_id"] == operation).collect();
                assert_eq!(matching.len(), 1, "one final failure for {operation}: {text}");
                let event = matching[0];
                assert_eq!(event["fields"]["stage"], "registration_completion");
                assert_eq!(event["fields"]["outcome"], if matches!(scenario, Scenario::Revoked) { "known_refusal" } else { "committed" });
                assert!(event["spans"].as_array().expect("spans").iter().any(|span| span["route"] == "/api/v1/registrations"));
                if matches!(scenario, Scenario::Replay) {
                    assert!(event["fields"]["commit"].as_str().expect("first original read commit").starts_with("Err(CommitTimeout(Elapsed("));
                    assert_eq!(event["fields"]["cleanup"], "Ok(())");
                    continue;
                }
                let cause = event["fields"]["error"].as_str().expect("original failure retained");
                assert!(cause.contains("Query(") || cause.contains("Timeout("), "{event}");
                let commit = event["fields"]["commit"].as_str().expect("read commit retained");
                let termination = event["fields"]["termination"].as_str().expect("driver result retained");
                match fault {
                    Fault::ReadCommitTermination => { assert_eq!(commit, "Ok(())"); assert!(termination.contains("database driver connection failed"), "{event}"); }
                    Fault::StalledReadCommit => assert!(commit.starts_with("Err(CommitTimeout("), "{event}"),
                    _ => assert!(commit.starts_with("Err(Commit("), "{event}"),
                }
            }
            for secret in ["fictional-sensitive-termination-detail", "Fixture Operator", "13800000000", "13900000000", "__Host-nucleus-session", "00000020000229007X", "000000200002290010", "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8", fixture::PASSWORD, "SELECT ", "INSERT "] {
                assert!(!events.iter().any(|event| contains_text(event, secret)), "private material in diagnostics: {secret}");
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("real driver observation").block_on(async {
            timeout(Duration::from_secs(90), async {
                for (index, (scenario, fault)) in cases.into_iter().enumerate() {
                    let name = empty_database(&server).await;
                    initialize(&server, &name, "13900000000").await;
                    let supplier = Supplier::echoing().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let proxy = Proxy::start(&server).await;
                    let application = Application::start(proxy_connections(&name, &proxy), keys(0), services(8).expect("settings"), process::Settings::new(Duration::from_secs(15)).expect("stop budget"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("application");
                    let (base, stop, mut server_task) = running(application).await;
                    let client = http_client();
                    let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
                    timeout(Duration::from_secs(3), async { loop {
                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND usename='clerk' AND state='idle' AND query LIKE 'SELECT family, operation_id, action, target, input, subject, person_id, revision_id, created_at, expires_at, seal, seal_key_id, seal_version FROM workforce.completions WHERE expires_at <= %')", &[]).await.expect("initial empty retention scan").get::<_, bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("background scan ends before fault selection");
                    let c1 = Uuid::new_v4().to_string();
                    let caller = tokio::spawn(client.post(format!("{base}/api/v1/codes")).header("operation-id", &c1).json(&serde_json::json!({"mobile":"13800000000", "token":c1})).send());
                    let sent = timeout(Duration::from_secs(3), sms.request()).await.expect("real C1 SMS");
                    let wire = std::str::from_utf8(&sent.bytes).expect("SMS request");
                    let target = wire.lines().next().expect("request line").split(' ').nth(1).expect("target");
                    let url = url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("supplier URL");
                    let template = url.query_pairs().find(|(key, _)| key == "TemplateParam").expect("template").1.into_owned();
                    let code = serde_json::from_str::<serde_json::Value>(&template).expect("template JSON")["code"].as_str().expect("code").to_owned();
                    accept_sms(sent);
                    let response = caller.await.expect("C1 caller").expect("C1 response"); assert_eq!(response.status(), 200);
                    let message = response.json::<serde_json::Value>().await.expect("message");
                    let response = client.post(format!("{base}/api/v1/sessions")).header("operation-id", &c1).json(&serde_json::json!({"secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8", "credential":{"type":"message", "id":message["message"]["id"], "mobile":"13800000000", "code":code}})).send().await.expect("R1");
                    assert_eq!(response.status(), 200);
                    let credential = response.json::<serde_json::Value>().await.expect("R1 credential")["registration"].clone();
                    let body = serde_json::json!({"id":credential["id"], "token":credential["token"], "name":"Fixture Operator", "idNumber":if matches!(scenario, Scenario::Occupied) { "000000200002290010" } else { "00000020000229007X" }});
                    let operation = format!("00000000-0000-4000-8000-{:012x}", index + 1);
                    let request = || client.post(format!("{base}/api/v1/registrations")).header("operation-id", &operation).json(&body).timeout(Duration::from_secs(25));
                    let previous = if matches!(scenario, Scenario::Replay | Scenario::Revoked) {
                        let response = request().send().await.expect("first committed registration"); assert_eq!(response.status(), 201);
                        let cookie = response.headers()["set-cookie"].to_str().expect("original cookie").to_owned();
                        let session = response.json::<serde_json::Value>().await.expect("original session");
                        Some((cookie, session))
                    } else { None };
                    let option = if matches!(scenario, Scenario::Revoked) {
                        let gate = connections(&server, &name, "clerk").recover().await.expect("original registration operation gate");
                        // Independent Python SHA-256 over LP32(registration/operation, UUID 00000000-0000-4000-8000-000000000006), first eight bytes.
                        gate.client().batch_execute("BEGIN; SELECT pg_catalog.pg_advisory_xact_lock(1776058838299520410)").await.expect("exact operation gate");
                        Some(gate)
                    } else { None };
                    proxy.arm(match scenario { Scenario::Replay => Fault::StalledReadCommit, Scenario::Revoked => Fault::StalledQuery, _ => Fault::DelayedCommit });
                    let mut caller = tokio::spawn(request().send());
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original commit intercepted");
                    if matches!(scenario, Scenario::Revoked) {
                        // Preflight must finish first; only the following business BEGIN fails.
                        proxy.arm(Fault::RejectedBegin); proxy.release();
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("business BEGIN failed after proof preflight");
                        let pid: i32 = option.as_ref().expect("gate").client().query_one("SELECT pg_backend_pid()", &[]).await.expect("gate PID").get(0);
                        timeout(Duration::from_secs(3), async { loop {
                            if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND mode='ExclusiveLock' AND classid=413520922::oid AND objid=2097753498::oid AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND $1=ANY(pg_catalog.pg_blocking_pids(pid)))", &[&pid]).await.expect("recovery waits for the exact original operation and backend").get::<_, bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }}).await.expect("locked recovery before logout");
                    }
                    if previous.is_none() {
                        timeout(Duration::from_secs(3), async { loop {
                            let waiting: bool = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND cardinality(pg_catalog.pg_blocking_pids(pid))>0)", &[]).await.expect("actual recovery waits for original transaction").get(0);
                            if waiting { break; }
                            tokio::task::yield_now().await;
                        }}).await.expect("recovery lock wait");
                    }
                    if matches!(scenario, Scenario::Revoked) {
                        let (cookie, session) = previous.as_ref().expect("original successful session");
                        let response = client.delete(format!("{base}/api/v1/sessions/current")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", session["session"]["id"].as_str().expect("session id")).header("cookie", cookie.split(';').next().expect("cookie pair")).send().await.expect("real logout before recovery qualification");
                        assert_eq!(response.status(), 200);
                    }
                    if !matches!(scenario, Scenario::Replay) {
                        proxy.arm(fault);
                        if let Some(gate) = option {
                            gate.client().batch_execute("COMMIT").await.expect("release recovery after logout");
                            gate.terminate().await.expect("gate ended");
                        } else { proxy.release(); }
                        timeout(Duration::from_secs(13), proxy.intercepted()).await.expect("readback cleanup fault");
                    }
                    let row = observer.client().query_one("SELECT attempt.status, attempt.credential_id=$2::text::uuid AND attempt.proof IS NOT NULL AND ((attempt.status='id_number_taken' AND receipt.id IS NULL AND registration.id=attempt.credential_id) OR (attempt.status='registered' AND receipt.id=attempt.id AND receipt.operation_id=attempt.operation_id AND receipt.proof=attempt.proof AND session.receipt_id=receipt.id AND session.created_at=attempt.accepted_at AND registration.id IS NULL)) AS paired, session.id AS session_id, session.expires_at, EXISTS(SELECT 1 FROM identity.handoffs WHERE id=session.id) AS handoff, (SELECT count(*) FROM identity.attempts WHERE operation_id=$1::text::uuid) AS attempts FROM identity.attempts attempt LEFT JOIN identity.receipts receipt ON receipt.operation_id=attempt.operation_id LEFT JOIN identity.sessions session ON session.receipt_id=receipt.id LEFT JOIN identity.registrations registration ON registration.id=attempt.credential_id WHERE attempt.operation_id=$1::text::uuid", &[&operation, &credential["id"].as_str().expect("credential id")]).await.expect("independent complete original graph before replay and drain");
                    assert!(row.get::<_, bool>("paired")); assert_eq!(row.get::<_, i64>("attempts"), 1);
                    assert_eq!(row.get::<_, bool>("handoff"), !matches!(scenario, Scenario::Occupied | Scenario::Revoked));
                    let original = if matches!(scenario, Scenario::Cancelled) {
                        caller.abort(); assert!((&mut caller).await.expect_err("HTTP wait cancelled").is_cancelled());
                        stop.send(()).expect("stop during accepted recovery");
                        assert!(timeout(Duration::from_millis(30), &mut server_task).await.is_err(), "work and recovery keep drain waiting");
                        None
                    } else {
                        let response = timeout(Duration::from_secs(13), &mut caller).await.expect("bounded recovery response").expect("caller").expect("response");
                        let expected = match scenario { Scenario::Replay => 200, Scenario::Occupied => 409, Scenario::Revoked => 410, _ => 201 };
                        assert_eq!(response.status(), expected);
                        let result = if matches!(scenario, Scenario::Occupied | Scenario::Revoked) {
                            assert!(!response.headers().contains_key("set-cookie"));
                            assert_eq!(response.json::<serde_json::Value>().await.expect("known refusal"), serde_json::json!({"code":if matches!(scenario, Scenario::Occupied) { "id_number_taken" } else { "verification_required" }}));
                            None
                        } else {
                            let cookie = response.headers()["set-cookie"].to_str().expect("recovered cookie").to_owned();
                            let session = response.json::<serde_json::Value>().await.expect("recovered session");
                            assert_eq!(session["session"]["id"].as_str().expect("session"), row.get::<_, Uuid>("session_id").to_string());
                            if let Some(previous) = previous { assert_eq!((&cookie, &session), (&previous.0, &previous.1)); }
                            Some((cookie, session))
                        };
                        stop.send(()).expect("stop after result");
                        result
                    };
                    let (application, result) = timeout(Duration::from_secs(15), server_task).await.expect("bounded drain").expect("server task"); result.expect("drained"); drop(application);
                    proxy.release();
                    timeout(Duration::from_secs(3), async { loop {
                        let ended: bool = observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename='clerk' AND pid<>pg_backend_pid())", &[]).await.expect("actual application backends ended").get(0);
                        if ended { break; } tokio::task::yield_now().await;
                    }}).await.expect("drivers and backends ended before restart");
                    let application = Application::start(connections(&server,&name,"clerk"), keys(0), services(8).expect("settings"), process::Settings::new(Duration::from_secs(5)).expect("stop budget"), supplier.client(),message_at(sms.address(),sms.authority())).await.expect("restart");
                    let (base, stop, task) = running(application).await;
                    let response = client.post(format!("{base}/api/v1/registrations")).header("operation-id", &operation).json(&body).send().await.expect("replay after confirmed stop");
                    if matches!(scenario, Scenario::Occupied | Scenario::Revoked) { assert_eq!(response.status(), if matches!(scenario, Scenario::Occupied) {409}else{410}); assert!(!response.headers().contains_key("set-cookie")); }
                    else {
                        assert_eq!(response.status(), 200);
                        let cookie = response.headers()["set-cookie"].to_str().expect("replay cookie").to_owned();
                        let session = response.json::<serde_json::Value>().await.expect("replay session");
                        assert_eq!(session["session"]["id"].as_str().expect("session"),row.get::<_,Uuid>("session_id").to_string());
                        if let Some(original) = original { assert_eq!((cookie,session),original); }
                    }
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.attempts WHERE operation_id=$1::text::uuid", &[&operation]).await.expect("no duplicate attempt").get::<_,i64>(0),1);
                    stop.send(()).expect("stop replay"); let (application,result)=task.await.expect("replay task"); result.expect("replay drained"); drop(application);
                    assert_eq!(supplier.calls.load(Ordering::SeqCst),1);
                    assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"R2 recovery, replay and drain issue no supplier request");
                    observer.terminate().await.expect("observer ended"); sms.stop().await; proxy.stop().await;
                }
            }).await.expect("HTTP readback scenario budget");
        });
    });
}

#[test]
fn http_disconnect_and_shutdown_preserve_the_single_send_and_original_result() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        let name = database(&server).await;
        let reader = connections(&server,&name,"clerk").recover().await.expect("trusted fixture time");
        let now: chrono::DateTime<chrono::Utc> = reader.client().query_one("SELECT clock_timestamp()", &[]).await.expect("time").get(0);
        reader.terminate().await.expect("reader ends");
        let supplier = Supplier::responding(serde_json::json!({"success":true,"challenge_ts":now,"hostname":"example.com","action":"send_authentication_code","cdata":OPERATION}).to_string()).await;
        let mut sms = fixture::supplier::Supplier::start().await;
        let proxy = fixture::database_proxy::Proxy::start(&server).await;
        let application = Application::start(proxy_connections(&name,&proxy), keys(0), services(8).expect("settings"), process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(),sms.authority())).await.expect("start");
        let (base,stop,mut task) = running(application).await;
        let client = http_client();
        let request = client.post(format!("{base}/api/v1/codes")).header("operation-id",OPERATION).header("x-real-ip","::ffff:192.0.2.1").json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"}));
        let caller = tokio::spawn(request.send());
        let sent = timeout(Duration::from_secs(4),sms.request()).await.expect("single PNVS call");
        caller.abort();
        assert!(caller.await.expect_err("waiter cancelled").is_cancelled());
        proxy.arm(fixture::database_proxy::Fault::StalledTime);
        // Python hmac.digest over the documented fields with the HKDF-SHA-256 `link` key of the
        // fixture root [1; 32] (salt "nucleus/key/v1"); expiry 2100-01-01.
        let image = tokio::spawn(client.get(format!("{base}/api/v1/avatars/a92a8f686199a7388e2132dd61643a8df504b13379771e147c8157e06fb197cb.webp?key_id=0&expires_at=4102444800&signature=eYBvLvmue1sKjyyqeJOI-dIObHu1OjfjqNKcM8ox4BU")).send());
        timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("image is preparing with the shared clock");
        stop.send(()).expect("stop with accepted send");
        assert!(timeout(Duration::from_millis(30),&mut task).await.is_err(), "shutdown must wait for the supplier conclusion");
        assert!(!image.is_finished(), "shutdown must not cancel the image's pending sample");
        accept_sms(sent);
        proxy.release();
        let (application,result) = timeout(Duration::from_secs(5),task).await.expect("stop deadline").expect("server task");
        result.expect("drained original send");
        assert!(image.await.expect("image caller").is_err(), "shutdown closes the image after its read finishes");
        assert!(instance_released(&server, &name).await);
        drop(application);
        let observer = connections(&server,&name,"clerk").recover().await.expect("independent observer before restart");
        let row = observer.client().query_one(
            "SELECT message.id, message.expires_at,
                challenge.status = 'passed' AND challenge.calls = 1
                AND message.status = 'sent' AND message.finished_at IS NOT NULL
                AND message.created_at >= challenge.finished_at
                AND verification.expires_at = message.expires_at
                AND verification.failures = 0 AND verification.authentication_id IS NULL
                AND claim.worker_id = message.worker_id
                AND claim.created_at >= message.created_at AND claim.created_at <= message.finished_at AS paired,
                (SELECT count(*) FROM identity.messages) AS messages,
                (SELECT count(*) FROM identity.checks) AS checks,
                (SELECT count(*) FROM identity.claims) AS claims
             FROM identity.challenges challenge
             JOIN identity.messages message ON message.challenge_id = challenge.id
             JOIN identity.checks verification ON verification.id = message.id
             JOIN identity.claims claim ON claim.id = message.id
             WHERE challenge.operation_id = $1::text::uuid", &[&OPERATION])
            .await.expect("original sent result is complete before any recovery can change it");
        assert!(row.get::<_,bool>("paired"));
        assert_eq!((row.get::<_,i64>("messages"),row.get::<_,i64>("checks"),row.get::<_,i64>("claims")),(1,1,1));
        let original_id: Uuid = row.get("id");
        let original_expiry: chrono::DateTime<chrono::Utc> = row.get("expires_at");
        observer.terminate().await.expect("independent observer ends");
        let application = Application::start(connections(&server,&name,"clerk"), keys(0), services(8).expect("settings"), process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(),sms.authority())).await.expect("restart");
        let (base,stop,task) = running(application).await;
        let replay = |source: &'static str, token: &'static str, mobile: &'static str| client.post(format!("{base}/api/v1/codes")).header("operation-id",OPERATION).header("x-real-ip",source).json(&serde_json::json!({"mobile":mobile,"token":token}));
        let response = replay("192.0.2.99","fictional-token","13800000000").send().await.expect("original recovery");
        assert_eq!(response.status(),200);
        let original: serde_json::Value = response.json().await.expect("original message");
        assert!(original.get("operationId").is_none());
        assert!(original.get("operation_id").is_none());
        assert_eq!(original.as_object().expect("response object").len(), 2);
        let message = &original["message"];
        assert_eq!(message.as_object().expect("message object").len(), 3);
        assert_eq!(message["id"].as_str().expect("message ID").parse::<Uuid>().expect("UUID"),original_id);
        assert_eq!(message["expiresAt"].as_str().expect("expiry").parse::<chrono::DateTime<chrono::Utc>>().expect("timestamp"),original_expiry);
        assert!(is_message_id(message["id"].as_str().expect("message UUID")));
        assert!(chrono::DateTime::parse_from_rfc3339(message["expiresAt"].as_str().expect("message expiry")).is_ok());
        assert!(message.get("expires_at").is_none());
        assert_eq!(original["message"]["delivery"],"sent");
        let response = replay("192.0.2.100","fictional-token","13800000000").send().await.expect("repeat recovery");
        assert_eq!(response.json::<serde_json::Value>().await.expect("same message"),original);
        assert_eq!(replay("192.0.2.101","wrong-token","13800000000").send().await.expect("wrong proof").status(),401);
        assert_eq!(replay("192.0.2.102","fictional-token","13900000000").send().await.expect("wrong mobile").status(),409);
        assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst),1);
        assert!(timeout(Duration::from_millis(50),sms.request()).await.is_err(),"recovery must not call PNVS");
        let reader = connections(&server,&name,"clerk").recover().await.expect("facts");
        let row = reader.client().query_one("SELECT (SELECT count(*) FROM identity.messages), (SELECT count(*) FROM identity.checks), (SELECT count(*) FROM identity.claims)",&[]).await.expect("pair and claim");
        assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1),row.get::<_,i64>(2)),(1,1,1));
        let row = reader.client().query_one("SELECT source::text FROM identity.challenges", &[]).await.expect("original source");
        assert_eq!(row.get::<_, String>(0), "192.0.2.1/32", "the proxy address and later recovery sources must not replace the original source");
        reader.terminate().await.expect("reader ends");
        stop.send(()).expect("stop replay process");
        let (application,result) = task.await.expect("server task");
        result.expect("drained");
        drop(application);
        assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst),1);
        assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err());
        sms.stop().await;
        proxy.stop().await;
    });
}

#[test]
fn background_retention_expires_identity_and_rechecks_late_requests() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        let supplier = Supplier::new().await;
        let name = database(&server).await;
        let mut application = start(&server, &name, 0, 8, &supplier).await;
        application.authentication().verify(input(OPERATION, "13800000000", "fictional-token"))
            .await.expect("retained verification result");
        application.stop().await.expect("original process drained");
        drop(application);
        let admin = connections(&server, &name, "postgres").recover().await.expect("isolated fixture admin");
        // Advance the fictional record's age while preserving its original proof and time relationships.
        admin.client().batch_execute("BEGIN; ALTER TABLE identity.challenges DISABLE TRIGGER USER;
            UPDATE identity.challenges SET created_at=created_at-interval '25 hours', expires_at=expires_at-interval '25 hours', finished_at=finished_at-interval '25 hours';
            ALTER TABLE identity.challenges ENABLE TRIGGER USER; COMMIT").await.expect("expired history fixture");
        let application = start(&server, &name, 0, 8, &supplier).await;
        let (base, stop, task) = running(application).await;
        timeout(Duration::from_secs(5), async {
            loop {
                let deleted: bool = admin.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM identity.challenges)", &[]).await.expect("deletion observer").get(0);
                if deleted { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("process triggers retention");
        let response = http_client().post(format!("{base}/api/v1/codes")).header("operation-id", OPERATION)
            .json(&serde_json::json!({"mobile":"13800000000", "token":"fictional-token"}))
            .send().await.expect("expired operation response");
        assert_eq!(response.status(), 403);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("operation-id"));
        assert_eq!(response.json::<serde_json::Value>().await.expect("response JSON"), serde_json::json!({"code":"verification_invalid"}));
        assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
        let messages: i64 = admin.client().query_one("SELECT count(*) FROM identity.messages", &[]).await.expect("no send permission").get(0);
        assert_eq!(messages, 0);
        stop.send(()).expect("stop");
        let (application, result) = task.await.expect("process task");
        result.expect("clean stop");
        drop(application);
        admin.terminate().await.expect("fixture admin ended");
    });
}

#[test]
fn session_and_registration_routes_share_admission_and_reject_before_business_work() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(20), async {
            let supplier = Supplier::new().await;
            let name = database(&server).await;
            let application = start(&server, &name, 0, 8, &supplier).await;
            let (base, stop, task) = running(application).await;
            let client = http_client();
            for path in ["/api/v1/sessions", "/api/v1/registrations"] {
                for (operation, source, origin, status) in [
                    ("invalid", "invalid", "https://attacker.invalid",400),
                    (OPERATION, "invalid", "https://attacker.invalid",400),
                    (OPERATION, "192.0.2.80", "https://attacker.invalid",422),
                    (OPERATION, "192.0.2.80", "https://example.com",422),
                ] {
                    let response = client.post(format!("{base}{path}")).header("operation-id",operation).header("x-real-ip",source).header("origin",origin).json(&serde_json::json!({})).send().await.expect("ordered boundary rejection");
                    assert_eq!(response.status(),status);
                    assert!(!response.headers().contains_key("set-cookie"));
                }
            }
            let begun = std::time::Instant::now();
            let mut requests = tokio::task::JoinSet::new();
            for index in 0..20 {
                let path = if index % 2 == 0 { "/api/v1/sessions" } else { "/api/v1/registrations" };
                requests.spawn(client.post(format!("{base}{path}")).header("operation-id",OPERATION).header("x-real-ip","192.0.2.199").json(&serde_json::json!({})).send());
            }
            let admitted = timeout(Duration::from_secs(5), async {
                let mut admitted = 0;
                while let Some(result) = requests.join_next().await {
                    let response = result.expect("request task").expect("shared admission response");
                    match response.status().as_u16() {
                        422 => admitted += 1,
                        429 => { assert!(response.headers().contains_key("retry-after")); },
                        status => panic!("unexpected status {status}"),
                    }
                }
                admitted
            }).await.expect("bounded simultaneous route burst");
            assert!(admitted >= 10);
            assert!(admitted <= 10 + begun.elapsed().as_secs(), "one shared ten-token bucket, refilled once per second");
            let begun = std::time::Instant::now();
            let mut requests = tokio::task::JoinSet::new();
            for index in 0..80 {
                let path = if index % 2 == 0 { "/api/v1/codes" } else { "/api/v1/registrations" };
                requests.spawn(client.post(format!("{base}{path}")).header("operation-id",OPERATION).header("x-real-ip",format!("203.0.113.{}",index+1)).json(&serde_json::json!({})).send());
            }
            let admitted = timeout(Duration::from_millis(900), async {
                let mut admitted = 0;
                while let Some(result) = requests.join_next().await {
                    let response = result.expect("total bucket task").expect("total bucket response");
                    match response.status().as_u16() {
                        422 => admitted += 1,
                        429 => { assert!(response.headers().contains_key("retry-after")); },
                        status => panic!("unexpected total bucket status {status}"),
                    }
                }
                admitted
            }).await.expect("eighty independent sources reach the shared total bucket within one second");
            assert!(begun.elapsed() < Duration::from_secs(1));
            let replenished = begun.elapsed().as_millis() * 20 / 1000;
            assert!(admitted <= 40 + replenished, "C1 and R2 debit the same forty-token total bucket");
            let observer = connections(&server,&name,"clerk").recover().await.expect("independent observer");
            assert_eq!(observer.client().query_one("SELECT (SELECT count(*) FROM identity.authentications)+(SELECT count(*) FROM identity.attempts)", &[]).await.expect("no business writes").get::<_,i64>(0),0);
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application,result)=task.await.expect("server task"); result.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),0);
        }).await.expect("route admission budget");
    });
}

#[test]
fn http_restart_resets_ingress_but_preserves_durable_message_quota() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        let name = database(&server).await;
        let mut sms = fixture::supplier::Supplier::start().await;
        let client = http_client();
        let mut original = serde_json::Value::Null;
        for operation_id in [OPERATION, OTHER] {
            let reader = connections(&server, &name, "clerk").recover().await.expect("fixture time");
            let now: chrono::DateTime<chrono::Utc> = reader.client().query_one("SELECT clock_timestamp()", &[]).await.expect("time").get(0);
            reader.terminate().await.expect("reader ends");
            let supplier = Supplier::responding(serde_json::json!({"success":true,"challenge_ts":now,"hostname":"example.com","action":"send_authentication_code","cdata":operation_id}).to_string()).await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0), services(8).expect("settings"), process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("start");
            let (base, stop, task) = running(application).await;
            let request = client.post(format!("{base}/api/v1/codes")).header("operation-id", operation_id).json(&serde_json::json!({"mobile":"13800000000","token":format!("fictional-{operation_id}")}));
            let response = if operation_id == OPERATION {
                let caller = tokio::spawn(request.send());
                accept_sms(timeout(Duration::from_secs(4), sms.request()).await.expect("first send"));
                caller.await.expect("caller").expect("HTTP response")
            } else {
                request.send().await.expect("durable quota response")
            };
            if operation_id == OPERATION {
                assert_eq!(response.status(), 200);
                original = response.json().await.expect("original message");
            } else {
                assert_eq!(response.status(), 429);
                let seconds: u64 = response.headers()["retry-after"].to_str().expect("seconds").parse().expect("integer");
                assert!((1..=60).contains(&seconds));
                assert_eq!(response.json::<serde_json::Value>().await.expect("quota JSON"), serde_json::json!({"code":"rate_limited"}));
                let replay = client.post(format!("{base}/api/v1/codes")).header("operation-id", OPERATION).json(&serde_json::json!({"mobile":"13800000000","token":format!("fictional-{OPERATION}")})).send().await.expect("original replay");
                assert_eq!(replay.status(), 200);
                assert_eq!(replay.json::<serde_json::Value>().await.expect("same message"), original);
                assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err());
            }
            assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            stop.send(()).expect("stop");
            let (application, result) = task.await.expect("HTTP task");
            result.expect("drained");
            drop(application);
        }
    });
}

pub(crate) async fn current_http(base: &str, cookie: &str, id: &str, suffix: &str) {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("anonymous fixture client");
    let cookie = cookie.split(';').next().expect("cookie pair");
    let (name, token) = cookie.split_once('=').expect("cookie name and token");
    let get = || {
        client
            .get(format!("{base}/api/v1/accounts/current"))
            .header("operation-id", OPERATION)
            .header("x-real-ip", "198.51.100.50")
    };
    let response = get()
        .header(
            "session-id",
            Uuid::parse_str(id)
                .expect("session ID")
                .simple()
                .to_string()
                .to_uppercase(),
        )
        .header(
            "cookie",
            format!("unrelated=ignored; {cookie}; __Host-nucleus-session-{OTHER}=ignored"),
        )
        .header("sec-fetch-site", "same-origin")
        .send()
        .await
        .expect("selected current account");
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("set-cookie"));
    let body: serde_json::Value = response.json().await.expect("complete projection");
    assert_eq!(body["code"], "ok");
    let account = &body["account"];
    assert_eq!(account.as_object().expect("account").len(), 7);
    assert_eq!(account["name"], "Fixture Operator");
    assert_eq!(account["idNumber"], serde_json::json!({"suffix":suffix}));
    assert!(account.as_object().expect("fields").contains_key("binding"));
    assert!(account["binding"].is_null());
    assert!(!body.to_string().contains(token));
    assert!(!body.to_string().contains("00000020000229"));
    let avatar = &account["avatar"];
    assert_eq!(avatar["width"], 512);
    assert_eq!(avatar["height"], 512);
    assert_eq!(avatar["mediaType"], "image/webp");
    let url = url::Url::parse(avatar["href"].as_str().expect("link")).expect("absolute signed URL");
    let digest = url
        .path()
        .strip_prefix("/api/v1/avatars/")
        .expect("versioned image route");
    assert!(digest.ends_with(".webp") && !digest.contains('/'));
    // The same signed proof is not accepted at the retired root path.
    let response = client
        .get(format!(
            "{base}/avatars/{digest}?{}",
            url.query().expect("proof")
        ))
        .header("operation-id", OPERATION)
        .send()
        .await
        .expect("retired image path");
    assert_eq!(response.status(), 404);
    assert_eq!(
        response
            .json::<serde_json::Value>()
            .await
            .expect("JSON 404"),
        serde_json::json!({"code":"resource_not_found"})
    );
    let image = format!("{base}{}?{}", url.path(), url.query().expect("proof"));
    let response = client
        .get(&image)
        .header("operation-id", "deliberately-invalid")
        .header("x-real-ip", "198.51.100.60")
        .header("authorization", "ignored-invalid-proof")
        .send()
        .await
        .expect("anonymous image");
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "image/webp");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["connection"], "close");
    let cache = response.headers()["cache-control"].to_str().expect("cache");
    assert!(
        cache.contains("private") && cache.contains("immutable") && !cache.contains("no-store")
    );
    let seconds: u64 = cache
        .split(", ")
        .find_map(|value| value.strip_prefix("max-age="))
        .expect("max age")
        .parse()
        .expect("seconds");
    assert!(seconds <= 86400);
    assert_eq!(
        response.bytes().await.expect("complete WebP").as_ref(),
        include_bytes!("../../assets/avatars/0-cat.webp")
    );
    let response = client
        .head(&image)
        .header("x-real-ip", "198.51.100.60")
        .send()
        .await
        .expect("image HEAD");
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["connection"], "close");
    assert!(response.bytes().await.expect("HEAD").is_empty());
    for query in [
        format!("{}&key_id=0", url.query().expect("proof")),
        format!("{}&extra=1", url.query().expect("proof")),
        "key_id=0&expires_at=1&signature=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        "key_id=00&expires_at=18446744073709551615&signature=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        url.query().expect("proof").replace("key_id=0", "key_id=%30"),
        url.query().expect("proof").replace("key_id=0", "key_id=+0"),
        format!("{}=", url.query().expect("proof")),
        "key_id=0&expires_at=18446744073709551616&signature=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
    ] {
        let response = client
            .get(format!("{base}{}?{query}", url.path()))
            .header("x-real-ip", "198.51.100.61")
            .send()
            .await
            .expect("invalid image proof");
        assert_eq!(response.status(), 403);
        assert_eq!(response.headers()["connection"], "close");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            response.json::<serde_json::Value>().await.expect("safe image rejection"),
            serde_json::json!({"code":"avatar_link_invalid"}),
        );
    }
    for (headers, status) in [
        (vec![], 401),
        (vec![("cookie", cookie.to_owned())], 401),
        (vec![("session-id", id.into())], 401),
        (
            vec![
                ("session-id", id.into()),
                ("cookie", format!("{cookie}; {cookie}")),
            ],
            400,
        ),
        (
            vec![
                ("session-id", id.into()),
                ("cookie", format!("{name}; {cookie}")),
            ],
            400,
        ),
        (
            vec![("session-id", id.into()), ("cookie", format!("{name}=bad"))],
            400,
        ),
        (
            vec![
                ("session-id", id.into()),
                (
                    "cookie",
                    format!("{name}=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
                ),
            ],
            401,
        ),
        (
            vec![
                ("session-id", id.into()),
                ("cookie", cookie.into()),
                ("authorization", format!("Bearer {token}")),
            ],
            400,
        ),
        (vec![("authorization", format!("Bearer {token}"))], 401),
        (vec![("authorization", String::new())], 400),
        (vec![("authorization", "Basic rejected".into())], 400),
        (
            vec![
                ("session-id", id.into()),
                ("cookie", cookie.into()),
                ("authorization", String::new()),
            ],
            400,
        ),
        (
            vec![
                ("session-id", id.into()),
                ("session-id", id.into()),
                ("cookie", cookie.into()),
            ],
            400,
        ),
        (
            vec![
                ("authorization", format!("Bearer {token}")),
                ("authorization", format!("Bearer {token}")),
            ],
            400,
        ),
    ] {
        let mut request = get().header("origin", "https://example.com");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .expect("credential selection rejection");
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("set-cookie"));
        assert_eq!(
            response
                .json::<serde_json::Value>()
                .await
                .expect("safe proof rejection"),
            serde_json::json!({ "code": if status == 400 { "invalid_input" } else { "authentication_invalid" } })
        );
    }
    for origins in [
        vec![],
        vec![("origin", "null")],
        vec![("origin", "https://sub.example.com")],
        vec![
            ("origin", "https://example.com"),
            ("origin", "https://example.com"),
        ],
        vec![("sec-fetch-site", "same-site")],
        vec![
            ("sec-fetch-site", "same-origin"),
            ("sec-fetch-site", "same-origin"),
        ],
        vec![
            ("origin", "https://example.com"),
            ("sec-fetch-site", "cross-site"),
        ],
    ] {
        let mut request = get().header("session-id", id).header("cookie", cookie);
        for (name, value) in origins {
            request = request.header(name, value);
        }
        assert_eq!(
            request.send().await.expect("read origin rejected").status(),
            403
        );
    }
    let response = get()
        .header("session-id", id)
        .header("cookie", cookie)
        .header("origin", "https://example.com")
        .send()
        .await
        .expect("current after rejections");
    assert_eq!(response.status(), 200);
}

/// Read the session's current account through the public website read, which never refreshes it.
pub(crate) async fn current_account(base: &str, identity: &(String, String)) -> serde_json::Value {
    let response = http_client()
        .get(format!("{base}/api/v1/accounts/current"))
        .header("operation-id", Uuid::new_v4().to_string())
        .header("session-id", &identity.0)
        .header("cookie", &identity.1)
        .header("x-real-ip", "198.51.100.251")
        .send()
        .await
        .expect("current account read");
    assert_eq!(response.status(), 200);
    let mut body: serde_json::Value = response.json().await.expect("current account");
    body["account"].take()
}

/// Parse one UUID field of a public response.
pub(crate) fn uuid_of(value: &serde_json::Value) -> Uuid {
    value
        .as_str()
        .expect("UUID text")
        .parse()
        .expect("UUID value")
}

pub(crate) async fn issued_session(
    base: &str,
    sms: &mut fixture::supplier::Supplier,
) -> (String, String) {
    issued_session_for(base, sms, "13800000000").await
}

pub(crate) async fn issued_session_for(
    base: &str,
    sms: &mut fixture::supplier::Supplier,
    mobile: &str,
) -> (String, String) {
    issued_session_from(base, sms, mobile, "198.51.100.90").await
}

pub(crate) async fn issued_session_from(
    base: &str,
    sms: &mut fixture::supplier::Supplier,
    mobile: &str,
    source: &str,
) -> (String, String) {
    issued_session_with_identity(base, sms, mobile, source, "00000020000229007X").await
}

pub(crate) async fn issued_session_with_identity(
    base: &str,
    sms: &mut fixture::supplier::Supplier,
    mobile: &str,
    source: &str,
    id_number: &str,
) -> (String, String) {
    issued_session_and_registration(base, sms, mobile, source, id_number)
        .await
        .0
}

pub(crate) struct RegistrationReplay {
    pub(crate) operation: Uuid,
    pub(crate) input: serde_json::Value,
}

pub(crate) async fn issued_session_and_registration(
    base: &str,
    sms: &mut fixture::supplier::Supplier,
    mobile: &str,
    source: &str,
    id_number: &str,
) -> ((String, String), Option<RegistrationReplay>) {
    let client = http_client();
    let operation = Uuid::new_v4().to_string();
    let mut request = tokio::spawn(
        client
            .post(format!("{base}/api/v1/codes"))
            .header("operation-id", &operation)
            .header("x-real-ip", source)
            .json(&serde_json::json!({"mobile":mobile,"token":operation}))
            .send(),
    );
    let sent = tokio::select! {
        response = &mut request => {
            let response = response.expect("code request task").expect("code response");
            let status = response.status();
            let body = response.json::<serde_json::Value>().await.expect("code error");
            panic!("code request completed before SMS: {status}, {}", body["code"]);
        }
        result = timeout(Duration::from_secs(3), sms.request()) => result.expect("actual SMS request"),
    };
    let text = std::str::from_utf8(&sent.bytes).expect("wire");
    let target = text
        .lines()
        .next()
        .expect("request line")
        .split(' ')
        .nth(1)
        .expect("target");
    let url = url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("URL");
    let template = url
        .query_pairs()
        .find(|(name, _)| name == "TemplateParam")
        .expect("template")
        .1
        .into_owned();
    let code = serde_json::from_str::<serde_json::Value>(&template).expect("template JSON")["code"]
        .as_str()
        .expect("code")
        .to_owned();
    accept_sms(sent);
    let response = request.await.expect("C1 caller").expect("C1 response");
    assert_eq!(response.status(), 200);
    let message = response.json::<serde_json::Value>().await.expect("message");
    let response=client.post(format!("{base}/api/v1/sessions")).header("operation-id",&operation).json(&serde_json::json!({"secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","credential":{"type":"message","id":message["message"]["id"],"mobile":mobile,"code":code}})).send().await.expect("R1");
    assert_eq!(response.status(), 200);
    let option = response.headers().get("set-cookie").cloned();
    let body = response
        .json::<serde_json::Value>()
        .await
        .expect("R1 result");
    if let Some(cookie) = option {
        return (
            (
                body["session"]["id"].as_str().expect("id").into(),
                cookie
                    .to_str()
                    .expect("cookie")
                    .split(';')
                    .next()
                    .expect("pair")
                    .into(),
            ),
            None,
        );
    }
    let replay = RegistrationReplay {
        operation: Uuid::new_v4(),
        input: serde_json::json!({"id":body["registration"]["id"],"token":body["registration"]["token"],"name":"Fixture Registered","idNumber":id_number}),
    };
    let response = client
        .post(format!("{base}/api/v1/registrations"))
        .header("operation-id", replay.operation.to_string())
        .json(&replay.input)
        .send()
        .await
        .expect("R2");
    assert_eq!(response.status(), 201);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .expect("cookie")
        .split(';')
        .next()
        .expect("pair")
        .to_owned();
    let body = response
        .json::<serde_json::Value>()
        .await
        .expect("R2 session");
    (
        (body["session"]["id"].as_str().expect("id").into(), cookie),
        Some(replay),
    )
}
