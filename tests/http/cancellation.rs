use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*, suspension::*};

#[test]
fn cancellation_recovers_each_operator_and_reports_one_safe_final_outcome() {
    use fixture::database_proxy::{Fault, Proxy};

    struct Coordination(Uuid);
    impl platform::cryptography::Input<2> for Coordination {
        fn fields(&self) -> [&[u8]; 2] {
            [b"identity/cancellation/operation", self.0.as_bytes()]
        }
    }

    if std::env::var_os("NUCLEUS_TEST_CANCELLATION_RECOVERY").is_none() {
        Runtime::new().expect("runtime").block_on(async {
            let expectations = tempfile::NamedTempFile::new().expect("private expected tokens");
            let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "cancellation::cancellation_recovers_each_operator_and_reports_one_safe_final_outcome", "--nocapture"])
                .env("NUCLEUS_TEST_CANCELLATION_RECOVERY", "1")
                .env("NUCLEUS_TEST_CANCELLATION_EXPECTATIONS", expectations.path())
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
                .kill_on_drop(true).spawn().expect("isolated diagnostics child");
            let output = timeout(Duration::from_secs(110), child.wait_with_output()).await.expect("child budget").expect("child result");
            let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
            assert!(output.status.success(), "{text}");
            let events: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).expect("JSON event")).collect();
            let failures: Vec<_> = events.iter().filter(|event| event["level"] == "ERROR").collect();
            assert_eq!(failures.len(), 8, "both operators recover cleanly or report each final failure once: {text}");
            for index in 0..10 {
                let scenario = index % 5;
                let operation = format!("00000000-0000-4000-a000-{:012x}", index + 1);
                let matching: Vec<_> = failures.iter().filter(|event| event["fields"]["operation_id"] == operation).collect();
                if scenario == 0 { assert!(matching.is_empty()); continue; }
                assert_eq!(matching.len(), 1, "one final diagnostic for {operation}");
                let event = matching[0];
                assert_eq!(event["fields"]["stage"], if matches!(scenario, 1 | 4) { "completion_recovery" } else { "command" });
                if matches!(scenario, 1 | 4) {
                    assert!(event["fields"]["kind"].is_null());
                } else {
                    assert_eq!(event["fields"]["kind"], if scenario == 3 { "unknown" } else { "unavailable" });
                }
                let route = if index < 5 { "/api/v1/accounts/current/cancellations" } else { "/api/v1/accounts/{id}/cancellations" };
                let span = event["spans"].as_array().expect("original spans").iter().find(|span| span["route"] == route).expect("original request");
                assert_eq!(span["method"], "POST"); assert_eq!(span["operation_id"], operation);
                Uuid::parse_str(span["request_id"].as_str().expect("request ID")).expect("UUID");
                let error = event["fields"]["error"].as_str().expect("safe retained cause");
                assert!(error.contains("stage: Commit, kind: Query(connection closed)"), "{event}");
                match scenario {
                    1 => {
                        assert_eq!(event["fields"]["outcome"], "committed");
                        assert!(!error.contains("(Commit, "));
                        assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(), 2);
                    }
                    2 => {
                        assert!(error.starts_with("Unavailable(Inner {"));
                        assert!(!error.contains("(Commit, ") && !error.contains("(Begin, "));
                        assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(), 1);
                    }
                    3 => { assert!(error.starts_with("Unknown(Inner {") && error.contains("(Begin, Query(")); }
                    4 => {
                        assert_eq!(event["fields"]["outcome"], "committed");
                        assert!(error.contains("(Commit, Query(connection closed))"));
                        assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(), 2);
                    }
                    _ => unreachable!("selected scenarios"),
                }
            }
            let tokens: Vec<String> = serde_json::from_slice(&std::fs::read(expectations.path()).expect("private token expectations")).expect("tokens");
            for secret in tokens.iter().map(String::as_str).chain(["13800000000", "13900000000", "Fixture Registered", "00000020000229007X", "__Host-nucleus-session", "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8", "fictional-sensitive-termination-detail", "203.0.146.1", fixture::PASSWORD, "SELECT ", "INSERT ", "DELETE "]) {
                assert!(!events.iter().any(|event| contains_text(event, secret)), "private material in diagnostic strings");
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("observe actual driver termination").block_on(async {
            timeout(Duration::from_secs(100), async {
                let mut tokens = Vec::new();
                for index in 0..10 {
                    let managed = index >= 5;
                    let scenario = index % 5;
                    let name = database(&server).await;
                    let supplier = Supplier::echoing().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let proxy = Proxy::start(&server).await;
                    let application = Application::start(
                        proxy_connections(&name, &proxy), keys(0),
                        services(8).expect("settings"),
                        process::Settings::new(Duration::from_secs(15)).expect("drain"),
                        supplier.client(), message_at(sms.address(), sms.authority()),
                    ).await.expect("verified startup");
                    let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
                    let (base, stop, mut task) = running(application).await;
                    timeout(Duration::from_secs(3), async { loop {
                        if observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid() AND usename = 'clerk' AND state = 'idle' AND query LIKE 'SELECT family, operation_id, action, target, input, subject, person_id, revision_id, created_at, expires_at, seal, seal_key_id, seal_version FROM workforce.completions WHERE expires_at <= %')", &[]).await.expect("initial retention completed").get::<_, bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("retention scan before fault");
                    let target = issued_session_for(&base, &mut sms, "13800000000").await;
                    tokens.push(target.1.split_once('=').expect("cookie pair").1.to_owned());
                    let operator = if managed { issued_session_for(&base, &mut sms, "13900000000").await } else { target.clone() };
                    tokens.push(operator.1.split_once('=').expect("operator cookie pair").1.to_owned());
                    let operator_revision_id: Uuid = observer.client().query_one("SELECT person.revision_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1::text::uuid", &[&operator.0]).await.expect("actual operator revision").get(0);
                    let row = observer.client().query_one("SELECT account.id, account.person_id, person.revision_id FROM identity.sessions session JOIN identity.accounts account ON account.id = session.account_id JOIN identity.persons person ON person.id = account.person_id WHERE session.id = $1::text::uuid", &[&target.0]).await.expect("current target");
                    let account_id: Uuid = row.get("id");
                    let person_id: Uuid = row.get("person_id");
                    let revision_id: Uuid = row.get("revision_id");
                    let input = if managed { serde_json::json!({"personId": person_id, "revisionId": revision_id, "mobile": "13800000000"}) } else { serde_json::json!({"accountId": account_id, "personId": person_id, "revisionId": revision_id}) };
                    let path = if managed { format!("/api/v1/accounts/{account_id}/cancellations") } else { "/api/v1/accounts/current/cancellations".to_owned() };
                    let operation = format!("00000000-0000-4000-a000-{:012x}", index + 1);
                    let unrelated = connections(&server, &name, "clerk").recover().await.expect("unrelated connection");
                    unrelated.client().batch_execute("BEGIN").await.expect("unrelated open transaction");
                    let unrelated_pid: i32 = unrelated.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("unrelated backend").get(0);
                    proxy.arm(match scenario { 0 => Fault::LostCommit, 2 => Fault::RolledBack, 3 => Fault::UnknownCommit, _ => Fault::DelayedCommit });
                    let mut caller = tokio::spawn(http_client().post(format!("{base}{path}"))
                        .header("operation-id", &operation).header("session-id", &operator.0).header("cookie", &operator.1)
                        .header("x-real-ip", "203.0.146.1").json(&input).timeout(Duration::from_secs(25)).send());
                    let original_pid = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("actual write COMMIT intercepted");
                    let mut stop = Some(stop);
                    if matches!(scenario, 1 | 4) {
                        let encoded = platform::cryptography::Encoded::new(&Coordination(Uuid::parse_str(&operation).expect("original operation"))).expect("original coordination encoding");
                        let digest = platform::cryptography::sha256::Digest::new(&encoded);
                        let classid = i64::from(u32::from_be_bytes(digest.as_ref()[..4].try_into().expect("lock high half")));
                        let objid = i64::from(u32::from_be_bytes(digest.as_ref()[4..8].try_into().expect("lock low half")));
                        timeout(Duration::from_secs(3), async { loop {
                            if observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity waiter JOIN pg_locks waiting ON waiting.pid=waiter.pid JOIN pg_stat_activity original ON original.pid = ANY(pg_blocking_pids(waiter.pid)) WHERE waiter.datname = current_database() AND original.datname = current_database() AND waiting.locktype='advisory' AND NOT waiting.granted AND waiting.classid=$2::bigint::oid AND waiting.objid=$3::bigint::oid AND waiting.objsubid=1 AND waiter.wait_event_type = 'Lock' AND waiter.query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock(%' AND original.state = 'idle in transaction' AND original.pid=$1)", &[&original_pid,&classid,&objid]).await.expect("independent recovery waits on the original operation behind its actual backend").get::<_, bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }}).await.expect("recovery reaches the original operation lock");
                        assert!(observer.client().query_one("SELECT EXISTS (SELECT 1 FROM identity.accounts WHERE id = $1) AND NOT EXISTS (SELECT 1 FROM identity.cancellations WHERE account_id = $1)", &[&account_id]).await.expect("original effects are still uncommitted").get::<_, bool>(0));
                        if scenario == 4 {
                            caller.abort(); assert!((&mut caller).await.expect_err("waiter cancelled").is_cancelled());
                            stop.take().expect("stop owner").send(()).expect("stop while accepted work remains");
                            assert!(timeout(Duration::from_millis(30), &mut task).await.is_err(), "accepted cancellation owns its work after HTTP disconnect");
                        }
                        proxy.arm(if scenario == 1 { Fault::ReadCommitTermination } else { Fault::LostReadCommit });
                        proxy.release();
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("verified recovery reaches its cleanup fault");
                    }
                    let response = if scenario == 4 { None } else {
                        let response = timeout(Duration::from_secs(12), &mut caller).await.expect("HTTP result budget").expect("caller").expect("HTTP response");
                        assert_eq!(response.status(), if matches!(scenario, 2 | 3) { 503 } else { 200 });
                        assert_eq!(response.headers()["cache-control"], "no-store");
                        assert_eq!(response.headers().contains_key("set-cookie"), !managed && matches!(scenario, 0 | 1));
                        let response = response.json::<serde_json::Value>().await.expect("safe response");
                        if scenario == 2 { assert_eq!(response, serde_json::json!({"code": "dependency_unavailable"})); }
                        if scenario == 3 {
                            assert_eq!(response, serde_json::json!({"code": "outcome_unknown"}));
                            assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.cancellations", &[]).await.expect("no known conclusion before late commit").get::<_, i64>(0), 0);
                        }
                        Some(response)
                    };
                    if matches!(scenario, 0 | 3 | 4) { proxy.release(); }
                    timeout(Duration::from_secs(3), async { loop {
                        if !observer.client().query_one(
                            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1)",
                            &[&original_pid],
                        ).await.expect("exact intercepted backend lifetime").get::<_, bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("original backend ended before independent final-state assertions");
                    assert!(observer.client().query_one(
                        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1 AND state='idle in transaction')",
                        &[&unrelated_pid],
                    ).await.expect("unrelated transaction remains open").get::<_, bool>(0));
                    if scenario == 2 {
                        assert!(observer.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM identity.cancellations) AND EXISTS (SELECT 1 FROM identity.accounts WHERE id = $1) AND EXISTS (SELECT 1 FROM identity.sessions WHERE account_id = $1) AND EXISTS (SELECT 1 FROM identity.receipts WHERE account_id = $1) AND EXISTS (SELECT 1 FROM access.holders WHERE id = $1) AND EXISTS (SELECT 1 FROM identity.avatars WHERE account_id = $1 AND released_at IS NULL) AND (SELECT sequence = 1 FROM identity.persons WHERE id = $2)", &[&account_id, &person_id]).await.expect("known rollback retains the complete current graph").get::<_, bool>(0));
                        let retry = http_client().post(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &operator.0).header("cookie", &operator.1).header("x-real-ip", "203.0.146.2").json(&input).send().await.expect("explicit new attempt rechecks the intact identity");
                        assert_eq!(retry.status(), 200);
                        assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.cancellations WHERE operation_id = $1::text::uuid", &[&operation]).await.expect("no automatic replay of the failed operation").get::<_, i64>(0), 0);
                    } else {
                        let fact = timeout(Duration::from_secs(3), async { loop {
                            if let Some(row) = observer.client().query_opt("SELECT id, cancelled_at, account_id = $2 AND person_id = $3 AND revision_id = $4 AND operator_id = $5 AND sequence = 2 AS bound, EXISTS (SELECT 1 FROM identity.accounts account JOIN identity.projections projection ON projection.account_id=account.id AND projection.id=account.cancellation_id WHERE account.id=$2 AND account.person_id=$3 AND account.cancellation_id=identity.cancellations.id AND account.mobile IS NULL AND account.id_number IS NULL AND account.suspension_id IS NULL AND account.sequence=2 AND account.version=account.sequence AND account.changed_at=identity.cancellations.cancelled_at AND projection.cause='cancellation' AND projection.person_id=account.person_id AND projection.sequence=account.sequence AND projection.projected_at=account.changed_at AND projection.operation_id=identity.cancellations.operation_id AND projection.transaction_id=identity.cancellations.transaction_id) AND NOT EXISTS (SELECT 1 FROM identity.sessions WHERE account_id = $2) AND NOT EXISTS (SELECT 1 FROM identity.receipts WHERE account_id = $2) AND EXISTS (SELECT 1 FROM access.holders holder JOIN access.projections projection ON projection.holder_id=holder.id AND projection.id=holder.cancellation_id WHERE holder.id=$2 AND projection.id=identity.cancellations.id AND holder.sequence=projection.sequence AND holder.changed_at=projection.projected_at AND projection.operation_id=identity.cancellations.operation_id) AND NOT EXISTS (SELECT 1 FROM identity.avatars WHERE account_id = $2 AND released_at IS NULL) AND (SELECT sequence = 2 FROM identity.persons WHERE id = $3) AS complete FROM identity.cancellations WHERE operation_id = $1::text::uuid", &[&operation, &account_id, &person_id, &revision_id, &operator_revision_id]).await.expect("independent durable outcome before replay or drain") { break row; }
                            tokio::task::yield_now().await;
                        }}).await.expect("original commit completes");
                        assert!(fact.get::<_, bool>("bound")); assert!(fact.get::<_, bool>("complete"));
                        if matches!(scenario, 0 | 1) {
                            assert_eq!(response.expect("known success"), serde_json::json!({"code": "ok", "cancellation": {"id": fact.get::<_, Uuid>("id"), "cancelledAt": fact.get::<_, chrono::DateTime<chrono::Utc>>("cancelled_at")}}));
                        }
                    }
                    if managed {
                        assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.sessions WHERE id=$1::text::uuid) AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$1::text::uuid)", &[&operator.0]).await.expect("the initiating manager session is retained in every recovery scenario").get::<_, bool>(0));
                    }
                    if let Some(stop) = stop { stop.send(()).expect("stop original server"); }
                    let (application, result) = timeout(Duration::from_secs(15), task).await.expect("accepted work drains").expect("server");
                    result.expect("normal drain"); drop(application);
                    assert_eq!(supplier.calls.load(Ordering::SeqCst), if managed { 2 } else { 1 });
                    assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err());
                    unrelated.client().batch_execute("ROLLBACK").await.expect("unrelated transaction ended");
                    unrelated.terminate().await.expect("unrelated connection ended");
                    observer.terminate().await.expect("observer ended"); sms.stop().await; proxy.stop().await;
                }
                std::fs::write(std::env::var_os("NUCLEUS_TEST_CANCELLATION_EXPECTATIONS").expect("private expectations path"), serde_json::to_vec(&tokens).expect("tokens JSON")).expect("private expectations");
            }).await.expect("bounded cancellation recovery scenarios");
        });
    });
}

#[test]
fn cancellation_work_capacity_rejects_both_actions_without_partial_effects() {
    use fixture::database_proxy::{Fault, Proxy};

    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        for managed in [false, true] {
            timeout(Duration::from_secs(35), async {
                let name = database(&server).await;
                let supplier = Supplier::echoing().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let application = Application::start(
                    proxy_connections(&name, &proxy), keys(0), services(1).expect("one work slot"),
                    process::Settings::new(Duration::from_secs(15)).expect("drain"),
                    supplier.client(), message_at(sms.address(), sms.authority()),
                ).await.expect("startup");
                let (base, stop, task) = running(application).await;
                let target = issued_session_for(&base, &mut sms, "13800000000").await;
                let operator = if managed { issued_session_for(&base, &mut sms, "13900000000").await } else { target.clone() };
                let account = current_account(&base, &target).await;
                let account_id = uuid_of(&account["id"]);
                let input = if managed {
                    serde_json::json!({"personId":account["personId"],"revisionId":account["revisionId"],"mobile":"13800000000"})
                } else {
                    serde_json::json!({"accountId":account_id,"personId":account["personId"],"revisionId":account["revisionId"]})
                };
                let path = if managed { format!("/api/v1/accounts/{account_id}/cancellations") } else { "/api/v1/accounts/current/cancellations".to_owned() };
                let original = Uuid::new_v4();
                let refused = Uuid::new_v4();
                let request = |operation: Uuid, source| http_client().post(format!("{base}{path}"))
                    .header("operation-id", operation.to_string())
                    .header("session-id", &operator.0).header("cookie", &operator.1)
                    .header("x-real-ip", source).json(&input);
                proxy.arm(Fault::DelayedCommit);
                let accepted = tokio::spawn(request(original, "203.0.147.21").send());
                let backend = timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("accepted cancellation COMMIT");
                let response = request(refused, "203.0.147.22").send().await.expect("shared capacity response");
                assert_eq!(response.status(), 429);
                assert_eq!(response.headers()["retry-after"], "1");
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("capacity result"), serde_json::json!({"code":"rate_limited"}));
                assert!(!accepted.is_finished());
                proxy.release();
                let response = timeout(Duration::from_secs(15), accepted).await.expect("accepted work finishes").expect("caller").expect("HTTP result");
                assert_eq!(response.status(), 200);
                assert_eq!(response.headers().contains_key("set-cookie"), !managed);
                let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
                timeout(Duration::from_secs(5), async {
                    while observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&backend]).await.expect("original backend").get::<_, bool>(0) {
                        tokio::task::yield_now().await;
                    }
                }).await.expect("original backend ended");
                assert!(observer.client().query_one(
                    "SELECT (SELECT count(*) FROM identity.cancellations WHERE operation_id=$1)=1
                        AND (SELECT count(*) FROM identity.completions WHERE family='cancellation' AND operation_id=$1)=1
                        AND NOT EXISTS(SELECT 1 FROM identity.cancellations WHERE operation_id=$2)
                        AND NOT EXISTS(SELECT 1 FROM identity.completions WHERE operation_id=$2)
                        AND NOT EXISTS(SELECT 1 FROM identity.sessions WHERE account_id=$3)",
                    &[&original, &refused, &account_id],
                ).await.expect("only accepted cancellation produces effects").get::<_, bool>(0));
                stop.send(()).expect("stop");
                let (application, result) = task.await.expect("server");
                result.expect("drained"); drop(application);
                observer.terminate().await.expect("observer ended");
                sms.stop().await; proxy.stop().await;
            }).await.expect("bounded capacity case");
        }
    });
}

#[test]
fn self_cancellation_requires_confirmation_and_only_clears_the_original_cookie() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(35), async {
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
            let (original, replay) = issued_session_and_registration(&base, &mut sms, "13800000000", "203.0.145.1", "00000020000229007X").await;
            let replay = replay.expect("real R2 completion");
            age_issuance(&server, &name, &issuance).await;
            let selected = issued_session_from(&base, &mut sms, "13800000000", "203.0.145.2").await;
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
            let target = current_account(&base, &selected).await;
            let account_id = uuid_of(&target["id"]);
            let person_id = uuid_of(&target["personId"]);
            let revision_id = uuid_of(&target["revisionId"]);
            let input = serde_json::json!({"accountId": account_id, "personId": person_id, "revisionId": revision_id});
            let operation = Uuid::new_v4();
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("no implicit Origin");
            let request = |source: u8| client.post(format!("{base}/api/v1/accounts/current/cancellations"))
                .header("operation-id", operation.to_string())
                .header("session-id", &selected.0).header("cookie", &selected.1)
                .header("x-real-ip", format!("203.0.145.{source}"));
            let mut invalid = input.clone(); invalid["accountId"] = serde_json::json!("private-invalid-id");
            let mut extra = input.clone(); extra["extra"] = serde_json::json!(true);
            let mut missing = input.clone(); missing.as_object_mut().expect("object").remove("revisionId");
            let mut changed = input.clone(); changed["revisionId"] = serde_json::json!(Uuid::new_v4());
            let mut cadence = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_millis(120), Duration::from_millis(120));
            for (builder, status, code) in [
                (request(3).header("sec-fetch-site", "same-origin").json(&input), 403, "permission_denied"),
                (request(4).header("origin", "https://other.example.com").json(&input), 403, "permission_denied"),
                (request(5).header("origin", "https://example.com").header("sec-fetch-site", "cross-site").json(&input), 403, "permission_denied"),
                (request(6).header("origin", "https://example.com").json(&invalid), 422, "invalid_input"),
                (request(7).header("origin", "https://example.com").json(&extra), 422, "invalid_input"),
                (request(8).header("origin", "https://example.com").json(&missing), 422, "invalid_input"),
                (request(9).header("origin", "https://example.com").json(&changed), 409, "target_changed"),
                (request(10).header("origin", "https://example.com").header("content-type", "application/json").body("{"), 400, "invalid_input"),
                (request(11).header("origin", "https://example.com").body("{}"), 415, "invalid_input"),
                (request(12).header("origin", "https://example.com").header("content-type", "application/json").body(" ".repeat(8193)), 413, "invalid_input"),
            ] {
                cadence.tick().await;
                let response = builder.send().await.expect("rejected input");
                assert_eq!(response.status(), status);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe error"), serde_json::json!({"code": code}));
            }
            assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.cancellations", &[]).await.expect("rejections make no completion").get::<_, i64>(0), 0);
            cadence.tick().await;
            let response = request(13).header("origin", "https://example.com").json(&input).send().await.expect("self cancellation");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store");
            let cookies: Vec<_> = response.headers().get_all("set-cookie").iter().collect();
            assert_eq!(cookies.len(), 1);
            let cookie = cookie::Cookie::parse(cookies[0].to_str().expect("cookie header")).expect("clear cookie");
            assert_eq!(cookie.name(), format!("__Host-nucleus-session-{}", selected.0));
            assert_eq!(cookie.value(), "");
            assert_eq!(cookie.expires_datetime().expect("expired cookie").unix_timestamp(), 0);
            assert_eq!(cookie.secure(), Some(true));
            assert_eq!(cookie.http_only(), Some(true));
            assert_eq!(cookie.path(), Some("/"));
            let result = response.json::<serde_json::Value>().await.expect("cancellation result");
            let fact = observer.client().query_one("SELECT id, cancelled_at, account_id = $2 AND person_id = $3 AND revision_id = $4 AND operator_id = $4 AND sequence=2 AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$3 AND sequence=2) AS bound, EXISTS (SELECT 1 FROM identity.accounts account JOIN identity.projections projection ON projection.account_id=account.id AND projection.id=account.cancellation_id WHERE account.id=$2 AND account.person_id=$3 AND account.cancellation_id=identity.cancellations.id AND account.mobile IS NULL AND account.id_number IS NULL AND account.suspension_id IS NULL AND account.sequence=2 AND account.version=account.sequence AND account.changed_at=identity.cancellations.cancelled_at AND projection.cause='cancellation' AND projection.person_id=account.person_id AND projection.sequence=account.sequence AND projection.projected_at=account.changed_at AND projection.operation_id=identity.cancellations.operation_id AND projection.transaction_id=identity.cancellations.transaction_id) AND NOT EXISTS (SELECT 1 FROM identity.sessions WHERE account_id = $2) AND NOT EXISTS (SELECT 1 FROM identity.receipts WHERE account_id = $2) AND EXISTS (SELECT 1 FROM access.holders holder JOIN access.projections projection ON projection.holder_id=holder.id AND projection.id=holder.cancellation_id WHERE holder.id=$2 AND projection.id=identity.cancellations.id AND holder.sequence=projection.sequence AND holder.changed_at=projection.projected_at AND projection.operation_id=identity.cancellations.operation_id) AS complete FROM identity.cancellations WHERE operation_id = $1", &[&operation, &account_id, &person_id, &revision_id]).await.expect("independent permanent completion before replay");
            assert!(fact.get::<_, bool>("bound")); assert!(fact.get::<_, bool>("complete"));
            assert_eq!(result, serde_json::json!({"code": "ok", "cancellation": {"id": fact.get::<_, Uuid>("id"), "cancelledAt": fact.get::<_, chrono::DateTime<chrono::Utc>>("cancelled_at")}}));
            for identity in [&original, &selected] {
                let response = http_client().post(format!("{base}/api/v1/accounts/current/cancellations"))
                    .header("operation-id", operation.to_string()).header("session-id", &identity.0).header("cookie", &identity.1)
                    .json(&input).send().await.expect("old session cannot query the completion");
                assert_eq!(response.status(), 401); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("invalid old session"), serde_json::json!({"code": "authentication_invalid"}));
            }
            let response = http_client().post(format!("{base}/api/v1/registrations")).header("operation-id", replay.operation.to_string()).json(&replay.input).send().await.expect("retained R2 original cannot reconstruct access");
            assert_eq!(response.status(), 410); assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("reverification"), serde_json::json!({"code": "verification_required"}));
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let path = &document["paths"]["/api/v1/accounts/current/cancellations"]["post"];
            for name in ["Operation-Id", "Session-Id", "Origin"] {
                assert_eq!(path["parameters"].as_array().expect("parameters").iter().find(|parameter| parameter["name"] == name).expect("required header")["required"], true);
            }
            let schema = &document["components"]["schemas"]["cancellation.Input"];
            for field in ["accountId", "personId", "revisionId"] {
                assert!(schema["required"].as_array().expect("confirmation required").contains(&serde_json::json!(field)));
                let reference = schema["properties"][field]["$ref"].as_str().expect("owned ID schema");
                assert_eq!(document.pointer(reference.strip_prefix('#').expect("local reference")).expect("ID schema")["format"], "uuid");
            }
            assert_eq!(schema["additionalProperties"], false);
            stop.send(()).expect("stop"); let (application, result) = task.await.expect("server ended"); result.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
            assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err());
            observer.terminate().await.expect("observer ended"); sms.stop().await;
        }).await.expect("bounded self-cancellation HTTP verification");
    });
}

#[test]
fn management_cancellation_preserves_the_operator_cookie_and_original_http_result() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        for paused in [false, true] {
            timeout(Duration::from_secs(35), async {
                let name = database(&server).await;
                let supplier = Supplier::echoing().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let application = Application::start(
                    connections(&server, &name, "clerk"), keys(0),
                    services(8).expect("settings"),
                    process::Settings::new(Duration::from_secs(5)).expect("stop"),
                    supplier.client(), message_at(sms.address(), sms.authority()),
                ).await.expect("verified startup");
                let (base, stop, task) = running(application).await;
                let manager = issued_session_for(&base, &mut sms, "13900000000").await;
                let target = issued_session_for(&base, &mut sms, "13800000000").await;
                let candidate = account_candidate(&base, &manager, "13800000000").await;
                let account_id = Uuid::parse_str(candidate["identity"]["accountId"].as_str().expect("account ID")).expect("UUID");
                let person_id = Uuid::parse_str(candidate["identity"]["personId"].as_str().expect("person ID")).expect("UUID");
                let revision_id = Uuid::parse_str(candidate["identity"]["revisionId"].as_str().expect("revision ID")).expect("UUID");
                let path = format!("/api/v1/accounts/{account_id}/cancellations");
                let input = serde_json::json!({"personId": person_id, "revisionId": revision_id, "mobile": "13800000000"});
                let operation = Uuid::new_v4();
                let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
                let operator = current_account(&base, &manager).await;
                let manager_id = uuid_of(&operator["id"]);
                let operator_revision_id = uuid_of(&operator["revisionId"]);
                let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("no implicit Origin");
                let request = |path: &str, actor: &(String, String), source: u8| client.post(format!("{base}{path}"))
                    .header("operation-id", operation.to_string()).header("session-id", &actor.0).header("cookie", &actor.1).header("x-real-ip", format!("203.0.147.{source}"));
                let mut invalid = input.clone(); invalid["personId"] = serde_json::json!("private-invalid-id");
                let mut extra = input.clone(); extra["accountId"] = serde_json::json!(account_id);
                let mut missing = input.clone(); missing.as_object_mut().expect("object").remove("mobile");
                let mut changed = input.clone(); changed["revisionId"] = serde_json::json!(Uuid::new_v4());
                let mut mobile = input.clone(); mobile["mobile"] = serde_json::json!("invalid-private-mobile");
                let mut cadence = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_millis(120), Duration::from_millis(120));
                for (builder, status, code) in [
                    (request(&path, &manager, 1).json(&input), 403, "permission_denied"),
                    (request(&path, &manager, 2).header("origin", "https://other.example.com").json(&input), 403, "permission_denied"),
                    (request(&path, &manager, 3).header("origin", "https://example.com").header("sec-fetch-site", "cross-site").json(&input), 403, "permission_denied"),
                    (request("/api/v1/accounts/invalid/cancellations", &manager, 4).header("origin", "https://example.com").json(&input), 400, "invalid_input"),
                    (request(&path, &manager, 5).header("origin", "https://example.com").json(&invalid), 422, "invalid_input"),
                    (request(&path, &manager, 6).header("origin", "https://example.com").json(&extra), 422, "invalid_input"),
                    (request(&path, &manager, 7).header("origin", "https://example.com").json(&missing), 422, "invalid_input"),
                    (request(&path, &manager, 8).header("origin", "https://example.com").json(&mobile), 422, "invalid_input"),
                    (request(&path, &manager, 9).header("origin", "https://example.com").json(&changed), 409, "target_changed"),
                    (request(&path, &target, 10).header("origin", "https://example.com").json(&input), 403, "permission_denied"),
                    (request(&format!("/api/v1/accounts/{manager_id}/cancellations"), &manager, 11).header("origin", "https://example.com").json(&input), 404, "target_unavailable"),
                    (request(&path, &manager, 12).header("origin", "https://example.com").header("content-type", "application/json").body("{"), 400, "invalid_input"),
                    (request(&path, &manager, 13).header("origin", "https://example.com").body("{}"), 415, "invalid_input"),
                    (request(&path, &manager, 14).header("origin", "https://example.com").header("content-type", "application/json").body(" ".repeat(8193)), 413, "invalid_input"),
                ] {
                    cadence.tick().await;
                    let response = builder.send().await.expect("rejected management input");
                    assert_eq!(response.status(), status); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe error"), serde_json::json!({"code": code}));
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.cancellations", &[]).await.expect("rejections write no completion").get::<_, i64>(0), 0);
                if paused {
                    suspend_account(&base, &manager, &candidate, "13800000000").await;
                    assert!(account_candidate(&base, &manager, "13800000000").await["suspensionId"].is_string());
                }
                cadence.tick().await;
                let response = request(&path, &manager, 15).header("origin", "https://example.com").json(&input).send().await.expect("management cancellation");
                assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                let body = response.json::<serde_json::Value>().await.expect("completion");
                let fact = observer.client().query_one("SELECT id, cancelled_at, person_id=$2 AND revision_id=$3 AND operator_id=$4 AND sequence=2 AND EXISTS(SELECT 1 FROM identity.persons WHERE id=$2 AND sequence=2) AS bound, EXISTS (SELECT 1 FROM identity.accounts account JOIN identity.projections projection ON projection.account_id=account.id AND projection.id=account.cancellation_id WHERE account.id=$6 AND account.person_id=$2 AND account.cancellation_id=identity.cancellations.id AND account.mobile IS NULL AND account.id_number IS NULL AND account.suspension_id IS NULL AND account.sequence=$5 AND account.version=account.sequence AND account.changed_at=identity.cancellations.cancelled_at AND projection.cause='cancellation' AND projection.person_id=account.person_id AND projection.sequence=account.sequence AND projection.projected_at=account.changed_at AND projection.operation_id=identity.cancellations.operation_id AND projection.transaction_id=identity.cancellations.transaction_id) AND NOT EXISTS(SELECT 1 FROM identity.sessions WHERE account_id=$6) AND EXISTS (SELECT 1 FROM access.holders holder JOIN access.projections projection ON projection.holder_id=holder.id AND projection.id=holder.cancellation_id WHERE holder.id=$6 AND projection.id=identity.cancellations.id AND holder.sequence=projection.sequence AND holder.changed_at=projection.projected_at AND projection.operation_id=identity.cancellations.operation_id) AND NOT EXISTS(SELECT 1 FROM identity.avatars WHERE account_id=$6 AND released_at IS NULL) AS complete FROM identity.cancellations WHERE operation_id=$1", &[&operation, &person_id, &revision_id, &operator_revision_id, &if paused {3_i64} else {2_i64}, &account_id]).await.expect("independent completion before replay");
                assert!(fact.get::<_, bool>("bound")); assert!(fact.get::<_, bool>("complete"));
                assert_eq!(body, serde_json::json!({"code": "ok", "cancellation": {"id": fact.get::<_, Uuid>("id"), "cancelledAt": fact.get::<_, chrono::DateTime<chrono::Utc>>("cancelled_at")}}));
                cadence.tick().await;
                let response = request(&path, &manager, 16).header("origin", "https://example.com").json(&input).send().await.expect("original result after cancellation");
                assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie")); assert_eq!(response.json::<serde_json::Value>().await.expect("original"), body);
                cadence.tick().await;
                let response = request(&path, &manager, 17).header("origin", "https://example.com").json(&changed).send().await.expect("old operation with changed confirmation");
                assert_eq!(response.status(), 409); assert!(!response.headers().contains_key("set-cookie")); assert_eq!(response.json::<serde_json::Value>().await.expect("bound original"), serde_json::json!({"code": "request_mismatch"}));
                let response = http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &manager.0).header("cookie", &manager.1).send().await.expect("manager access after cancellation");
                assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie"));
                let response = http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &target.0).header("cookie", &target.1).send().await.expect("old target session");
                assert_eq!(response.status(), 401);
                let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
                let route = &document["paths"]["/api/v1/accounts/{id}/cancellations"]["post"];
                for name in ["id", "Operation-Id", "Session-Id", "Origin"] {
                    assert_eq!(route["parameters"].as_array().expect("parameters").iter().find(|parameter| parameter["name"] == name).expect("required parameter")["required"], true);
                }
                assert!(route["responses"]["200"]["headers"].get("Set-Cookie").is_none());
                let schema = &document["components"]["schemas"]["cancellation.Management"];
                assert_eq!(schema["additionalProperties"], false);
                for field in ["personId", "revisionId", "mobile"] { assert!(schema["required"].as_array().expect("required confirmation").contains(&serde_json::json!(field))); }
                assert_eq!(document["paths"]["/api/v1/accounts/candidates"]["post"]["operationId"], "accountCandidate");
                stop.send(()).expect("stop"); let (application, result) = task.await.expect("server ended"); result.expect("drained"); drop(application);
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 2); assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err());
                observer.terminate().await.expect("observer ended"); sms.stop().await;
            }).await.expect("bounded management HTTP contract");
        }
    });
}
