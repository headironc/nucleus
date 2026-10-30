use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*};

#[test]
fn website_logout_uses_the_selected_cookie_and_reads_its_original_with_another_session() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(45),async {
            for (registration, all) in [(false,false),(true,false),(true,true)] {
                let name=empty_database(&server).await;
                initialize(&server,&name,if registration {"13900000000"} else {"13800000000"}).await;
                let supplier=Supplier::echoing().await;let mut sms=fixture::supplier::Supplier::start().await;
                let proxy=fixture::database_proxy::Proxy::start(&server).await;
                let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("start");
                let issuance = application.registration().clone();
                let (base,stop,task)=running(application).await;
                let (id,cookie)=issued_session(&base,&mut sms).await;
                age_issuance(&server, &name, &issuance).await;
                stop.send(()).expect("stop before advancing to another issuance");
                let (application,result)=task.await.expect("first server");result.expect("first graph drained");drop(application);
                let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("restart preserves the first session");
                let issuance = application.registration().clone();
                let (base,stop,task)=running(application).await;
                let (other,other_cookie)=issued_session(&base,&mut sms).await;
                let client=reqwest::Client::new();let operation=Uuid::new_v4().to_string();
                let path = if all {"/api/v1/sessions"} else {"/api/v1/sessions/current"};
                for (index,(headers,body,status)) in [
                    (vec![("sec-fetch-site","same-origin")],"",403),
                    (vec![("origin","https://attacker.invalid")],"",403),
                    (vec![("origin","https://example.com"),("origin","https://example.com")],"",403),
                    (vec![("origin","https://example.com")],"{}",400),
                    (vec![("origin","https://example.com"),("authorization","Bearer AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8")],"",400),
                ].into_iter().enumerate() {
                    let mut request=client.delete(format!("{base}{path}")).header("operation-id",&operation).header("session-id",&id).header("cookie",&cookie).header("x-real-ip",format!("203.0.113.{}",index+1)).body(body);
                    for (name,value) in headers {request=request.header(name,value);}
                    let response=request.send().await.expect("safe refusal");assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie"));
                }
                let observer=connections(&server,&name,"clerk").recover().await.expect("observer");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.revocations",&[]).await.expect("no refused writes").get::<_,i64>(0),0);
                proxy.arm(fixture::database_proxy::Fault::LostCommit);
                let pending=tokio::spawn(client.delete(format!("{base}{path}")).header("operation-id",&operation).header("session-id",&id).header("cookie",format!("{other_cookie}; {cookie}")).header("origin","https://example.com").header("x-real-ip","203.0.113.50").send());
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("logout COMMIT response loss");proxy.release();
                let response=pending.await.expect("caller").expect("recovered logout");assert_eq!(response.status(),200);assert_eq!(response.headers()["cache-control"],"no-store");
                let clear=cookie::Cookie::parse(response.headers()["set-cookie"].to_str().expect("clear cookie").to_owned()).expect("cookie syntax");
                assert_eq!(clear.name(),format!("__Host-nucleus-session-{id}"));assert_eq!(clear.value(),"");assert_eq!(clear.path(),Some("/"));assert_eq!(clear.secure(),Some(true));assert_eq!(clear.http_only(),Some(true));assert_eq!(clear.same_site(),Some(cookie::SameSite::Lax));assert!(clear.domain().is_none());assert!(clear.max_age().is_none());assert_eq!(clear.expires_datetime(),Some(cookie::time::OffsetDateTime::UNIX_EPOCH));
                let result=response.json::<serde_json::Value>().await.expect("completion");assert_eq!(result["completion"]["id"],operation);assert_eq!(result["completion"]["count"],if all {2}else{1});
                let row=observer.client().query_one("SELECT operation_id::text,revoked_at,NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=revocation.id) AS removed,EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$2::text::uuid) AS other,(SELECT count(*) FROM identity.revocations WHERE operation_id=$3::text::uuid) AS count FROM identity.revocations revocation WHERE id=$1::text::uuid",&[&id,&other,&operation]).await.expect("independent original before replay");assert_eq!(row.get::<_,String>(0),operation);assert!(row.get::<_,bool>("removed"));assert_eq!(row.get::<_,bool>("other"),!all);assert_eq!(row.get::<_,i64>("count"),if all{2}else{1});assert_eq!(row.get::<_,chrono::DateTime<chrono::Utc>>("revoked_at"),chrono::DateTime::parse_from_rfc3339(result["completion"]["revokedAt"].as_str().expect("timestamp")).expect("timestamp").with_timezone(&chrono::Utc));
                let mut expected = vec![serde_json::json!({"id":id,"revokedAt":result["completion"]["revokedAt"]})];
                if all { expected.push(serde_json::json!({"id":other,"revokedAt":result["completion"]["revokedAt"]})); }
                expected.sort_by(|left,right|left["id"].as_str().cmp(&right["id"].as_str()));
                let logout_query = "SELECT logout.revoked_at,logout.sessions,logout.account_id=session.account_id AND logout.person_id=account.person_id AND logout.initiator_id=$2::text::uuid AND logout.scope=$3 AS bound FROM identity.logouts logout JOIN identity.sessions session ON session.id=logout.initiator_id JOIN identity.accounts account ON account.id=logout.account_id WHERE logout.id=$1::text::uuid";
                let scope = if all { "all" } else { "current" };
                let original = observer.client().query_one(logout_query,&[&operation,&id,&scope]).await.expect("permanent original logout before fixture aging");
                assert!(original.get::<_,bool>("bound"));
                let original_time = original.get::<_,chrono::DateTime<chrono::Utc>>("revoked_at");
                let original_sessions = original.get::<_,Vec<Uuid>>("sessions");
                assert_eq!(original_time,row.get::<_,chrono::DateTime<chrono::Utc>>("revoked_at"));
                assert_eq!(original_sessions.iter().map(|id|id.to_string()).collect::<Vec<_>>(),expected.iter().map(|value|value["id"].as_str().expect("session ID").to_owned()).collect::<Vec<_>>());
                for (selected,pair,status) in [(&id,&cookie,401),(&other,&other_cookie,if all{401}else{409})] {
                    let response=client.delete(format!("{base}{path}")).header("operation-id",&operation).header("session-id",selected).header("cookie",pair).header("origin","https://example.com").header("x-real-ip","203.0.113.51").send().await.expect("duplicate DELETE");assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie"));
                }
                let (other,other_cookie) = if all {age_issuance(&server,&name, &issuance).await;issued_session(&base,&mut sms).await} else {(other,other_cookie)};
                let aged = observer.client().query_one(logout_query,&[&operation,&id,&scope]).await.expect("independent original logout after fixture aging");
                assert!(aged.get::<_,bool>("bound"));
                assert_eq!(aged.get::<_,Vec<Uuid>>("sessions"),original_sessions);
                // Only the fixture's stored timeline moves; the original logout set stays fixed.
                let expected_time = original_time - chrono::TimeDelta::seconds(if all {61} else {0});
                assert_eq!(aged.get::<_,chrono::DateTime<chrono::Utc>>("revoked_at"),expected_time);
                if all {assert!(!original_sessions.contains(&Uuid::parse_str(&other).expect("new session ID")));}
                expected = aged.get::<_,Vec<Uuid>>("sessions").iter().map(|id|serde_json::json!({"id":id,"revokedAt":aged.get::<_,chrono::DateTime<chrono::Utc>>("revoked_at")})).collect();
                let response=client.get(format!("{base}/api/v1/revocations?operation_id={operation}")).header("operation-id",OTHER).header("session-id",&other).header("cookie",&other_cookie).header("sec-fetch-site","same-origin").header("x-real-ip","203.0.113.52").send().await.expect("original GET");assert_eq!(response.status(),200);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("original set"),serde_json::json!({"code":"ok","original":{"scope":scope,"initiatorId":id},"count":expected.len(),"revocations":expected}));
                let response=client.head(format!("{base}/api/v1/revocations?operation_id={operation}")).header("operation-id",OTHER).header("session-id",&other).header("cookie",&other_cookie).header("sec-fetch-site","same-origin").header("x-real-ip","203.0.113.52").send().await.expect("original HEAD");
                assert_eq!(response.status(),200);assert!(!response.headers().contains_key("set-cookie"));assert!(response.bytes().await.expect("HEAD body").is_empty());
                for query in ["", "operation_id=invalid", "operation_id=00000000-0000-4000-8000-000000000001&operation_id=00000000-0000-4000-8000-000000000002", "operation_id=00000000-0000-4000-8000-000000000001&extra=1"] {
                    let response=client.get(format!("{base}/api/v1/revocations?{query}")).header("operation-id",OTHER).header("session-id",&other).header("cookie",&other_cookie).header("sec-fetch-site","same-origin").header("x-real-ip","203.0.113.52").send().await.expect("invalid query");
                    assert_eq!(response.status(),400);assert!(!response.headers().contains_key("set-cookie"));
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.revocations",&[]).await.expect("reads do not write").get::<_,i64>(0),if all{2}else{1});
                observer.terminate().await.expect("observer ended");
                stop.send(()).expect("stop");let (application,result)=task.await.expect("server");result.expect("drained");drop(application);
                assert_eq!(supplier.calls.load(Ordering::SeqCst),if all{3}else{2});assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err());
                drop(supplier);sms.stop().await;proxy.stop().await;
            }
        }).await.expect("HTTP logout budget");
    });
}

#[test]
fn abandoned_logout_keeps_shutdown_waiting_and_records_one_safe_final_failure() {
    if std::env::var_os("NUCLEUS_TEST_LOGOUT_DIAGNOSTICS").is_none() {
        Runtime::new().expect("runtime").block_on(async {
            let child=tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact","revocation::abandoned_logout_keeps_shutdown_waiting_and_records_one_safe_final_failure","--nocapture"])
                .env("NUCLEUS_TEST_LOGOUT_DIAGNOSTICS","1").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().expect("isolated logging process");
            let output=timeout(Duration::from_secs(30),child.wait_with_output()).await.expect("child budget").expect("child result");
            let text=String::from_utf8(output.stderr).expect("safe diagnostics");assert!(output.status.success(),"{text}");
            let events:Vec<serde_json::Value>=text.lines().map(|line|serde_json::from_str(line).expect("JSON event")).collect();
            let completions:Vec<_>=events.iter().filter(|event|event["level"]=="ERROR"&&event["fields"]["operation_id"]==OTHER).collect();
            assert_eq!(completions.len(),1,"one final failure for the abandoned operation: {text}");
            let event=completions[0];assert_eq!(event["fields"]["stage"],"command");assert_eq!(event["fields"]["kind"],"unavailable");
            let error=event["fields"]["error"].as_str().expect("typed clock failure");
            assert!(error.starts_with("Unavailable(Inner { stage: Time, kind: Clock(ClockInvalid)"),"{event}");
            assert!(event["spans"].as_array().expect("request spans").iter().any(|span|span["route"]=="/api/v1/sessions/current"&&span["operation_id"]==OTHER));
            for secret in ["13800000000","__Host-nucleus-session","Bearer ","SELECT ","UPDATE "] {assert!(!events.iter().any(|event|contains_text(event,secret)),"private material in diagnostics");}
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20),async {
                let name=database(&server).await;
                let supplier=Supplier::echoing().await;let mut sms=fixture::supplier::Supplier::start().await;
                let proxy=fixture::database_proxy::Proxy::start(&server).await;
                let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(4).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop budget"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("application");
                let (base,stop,mut task)=running(application).await;
                let (id,cookie)=issued_session(&base,&mut sms).await;
                let owner=connections(&server,&name,"postgres").recover().await.expect("isolated blocker");
                let blocker:i32=owner.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("blocker identity").get(0);
                owner.client().batch_execute("BEGIN; SELECT id FROM identity.accounts FOR UPDATE").await.expect("block accepted logout");
                let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
                let caller=tokio::spawn(http_client().delete(format!("{base}/api/v1/sessions/current")).header("operation-id",OTHER).header("session-id",&id).header("cookie",&cookie).send());
                timeout(Duration::from_secs(3),async {loop {
                    let waiting:bool=observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND usename='clerk' AND wait_event_type='Lock' AND query=$1 AND $2=ANY(pg_blocking_pids(pid)))",&[&"SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE",&blocker]).await.expect("actual accepted work blocked by the owner").get(0);
                    if waiting {break;} tokio::task::yield_now().await;
                }}).await.expect("logout waits on the account");
                caller.abort();assert!(caller.await.expect_err("caller cancelled").is_cancelled());
                stop.send(()).expect("stop with accepted logout");
                assert!(timeout(Duration::from_millis(30),&mut task).await.is_err(),"stop cannot release running write work");
                proxy.arm(fixture::database_proxy::Fault::RewoundCurrent);
                owner.client().batch_execute("COMMIT").await.expect("release the accepted logout");
                timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("logout observes rewound time");
                proxy.release();
                let (application,result)=timeout(Duration::from_secs(5),task).await.expect("stop budget").expect("supervisor");result.expect("drained failed work");drop(application);
                let row=observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.revocations) AS unchanged, EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$1::text::uuid) AS retained",&[&id]).await.expect("independent result before any recovery");
                assert!(row.get::<_,bool>("unchanged")&&row.get::<_,bool>("retained"));
                observer.terminate().await.expect("observer ended");owner.terminate().await.expect("owner ended");
                assert_eq!(supplier.calls.load(Ordering::SeqCst),1);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err());sms.stop().await;
                proxy.stop().await;
            }).await.expect("logout shutdown budget");
        });
    });
}

#[test]
fn logout_result_read_keeps_verified_sets_and_logs_cleanup_once() {
    use fixture::database_proxy::{Fault, Proxy};

    if std::env::var_os("NUCLEUS_TEST_LOGOUT_READ").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let expected = tempfile::NamedTempFile::new().expect("private expected tokens");
            let child =
                tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args([
                        "--exact",
                        "revocation::logout_result_read_keeps_verified_sets_and_logs_cleanup_once",
                        "--nocapture",
                    ])
                    .env("NUCLEUS_TEST_LOGOUT_READ", "1")
                    .env("NUCLEUS_TEST_LOGOUT_TOKENS", expected.path())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .expect("isolated diagnostics process");
            let output = timeout(Duration::from_secs(40), child.wait_with_output())
                .await
                .expect("child budget")
                .expect("child result");
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
            assert_eq!(failures.len(), 2, "one cleanup diagnostic per read: {text}");
            for (original, request_operation, timeout) in [
                (OPERATION, "00000000-0000-4000-8000-000000000101", false),
                (
                    "00000000-0000-4000-8000-000000000001",
                    "00000000-0000-4000-8000-000000000102",
                    true,
                ),
            ] {
                assert_ne!(
                    request_operation, original,
                    "the read identity is not its original logout query"
                );
                let matching: Vec<_> = failures
                    .iter()
                    .filter(|event| {
                        event["spans"].as_array().is_some_and(|spans| {
                            spans.iter().any(|span| {
                                span["route"] == "/api/v1/revocations"
                                    && span["operation_id"] == request_operation
                            })
                        })
                    })
                    .collect();
                assert_eq!(
                    matching.len(),
                    1,
                    "each read has one identified cleanup failure: {text}"
                );
                let event = matching[0];
                assert_eq!(event["fields"]["stage"], "read_commit");
                assert_eq!(event["fields"]["outcome"], "known");
                let error = event["fields"]["error"]
                    .as_str()
                    .expect("typed read end and cleanup causes");
                if timeout {
                    assert!(
                        error.starts_with("Inner { stage: Read, kind: Timeout(Elapsed("),
                        "{event}"
                    );
                    assert!(
                        error.contains("later: []"),
                        "the stalled live driver ended without another failure: {event}"
                    );
                } else {
                    assert!(
                        error.starts_with("Inner { stage: Read, kind: Query(connection closed)"),
                        "{event}"
                    );
                    assert!(
                        error.contains(
                            "(Termination, Connection(database driver connection failed))"
                        ),
                        "actual driver failure remains attached: {event}"
                    );
                }
                let span = event["spans"]
                    .as_array()
                    .expect("request spans")
                    .iter()
                    .find(|span| span["route"] == "/api/v1/revocations")
                    .expect("read request span");
                assert_eq!(
                    span["operation_id"], request_operation,
                    "header identifies this read, not the original logout"
                );
                let request_id = span["request_id"].as_str().expect("request identity");
                Uuid::parse_str(request_id).expect("request UUID");
                let response = events
                    .iter()
                    .find(|event| {
                        event["fields"]["stage"] == "http_response"
                            && event["spans"].as_array().is_some_and(|spans| {
                                spans.iter().any(|span| span["request_id"] == request_id)
                            })
                    })
                    .expect("same request response");
                assert_eq!(response["fields"]["status"], 200);
            }
            let tokens: Vec<String> =
                serde_json::from_slice(&std::fs::read(expected.path()).expect("private tokens"))
                    .expect("tokens");
            for secret in tokens.iter().map(String::as_str).chain([
                "13800000000",
                "__Host-nucleus-session",
                fixture::PASSWORD,
                "SELECT ",
                "fictional-sensitive-termination-detail",
            ]) {
                assert!(
                    !events.iter().any(|event| contains_text(event, secret)),
                    "private material in diagnostics"
                );
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(35), async {
                let name = database(&server).await;
                let supplier = Supplier::echoing().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let application = Application::start(proxy_connections(&name, &proxy), keys(0),
                    services(4).expect("settings"),
                    process::Settings::new(Duration::from_secs(20)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("application");
                let issuance = application.registration().clone();
                let (base, stop, task) = running(application).await;
                let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
                timeout(Duration::from_secs(3), async { loop {
                    if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND usename='clerk' AND state='idle' AND query LIKE 'SELECT family, operation_id, action, target, input, subject, person_id, revision_id, created_at, expires_at, seal, seal_key_id, seal_version FROM workforce.completions WHERE expires_at <= %')", &[]).await.expect("last empty scan").get::<_, bool>(0) { break; }
                    tokio::task::yield_now().await;
                }}).await.expect("initial background round ends before fault injection");
                let (id, cookie) = issued_session(&base, &mut sms).await;
                age_issuance(&server, &name, &issuance).await;
                let (reader, reader_cookie) = issued_session(&base, &mut sms).await;
                std::fs::write(std::env::var_os("NUCLEUS_TEST_LOGOUT_TOKENS").expect("private token path"), serde_json::to_vec(&[cookie.split_once('=').expect("original cookie").1, reader_cookie.split_once('=').expect("reader cookie").1]).expect("token fixture")).expect("parent-owned private fixture");
                let client = http_client();
                let response = client.delete(format!("{base}/api/v1/sessions/current")).header("operation-id", OPERATION).header("session-id", &id).header("cookie", &cookie).send().await.expect("logout");
                assert_eq!(response.status(), 200);
                let query = "SELECT row_to_json(revocation)::text AS original, revoked_at, NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=revocation.id) AS removed FROM identity.revocations revocation WHERE operation_id=$1::text::uuid";
                let row = observer.client().query_one(query, &[&OPERATION]).await.expect("independent original result before GET");
                assert!(row.get::<_, bool>("removed"));
                let original: String = row.get("original");
                let revoked_at: chrono::DateTime<chrono::Utc> = row.get("revoked_at");
                for (original_operation, request_operation, replay_operation, fault) in [(OPERATION, "00000000-0000-4000-8000-000000000101", "00000000-0000-4000-8000-000000000201", Fault::LostReadCommit), ("00000000-0000-4000-8000-000000000001", "00000000-0000-4000-8000-000000000102", "00000000-0000-4000-8000-000000000202", Fault::StalledReadCommit)] {
                    let request = |request_operation:&str| client.get(format!("{base}/api/v1/revocations?operation_id={original_operation}")).header("operation-id", request_operation).header("session-id", &reader).header("cookie", &reader_cookie).timeout(Duration::from_secs(15)).send();
                    proxy.arm(fault);
                    let caller = tokio::spawn(request(request_operation));
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("read COMMIT intercepted after qualification and complete projection");
                    let response = tokio::select! {
                        response = caller => response.expect("read task").expect("read response"),
                        _ = sms.request() => panic!("result reads cannot send SMS"),
                    };
                    assert_eq!(response.status(), 200);
                    assert_eq!(response.headers()["cache-control"], "no-store");
                    assert!(!response.headers().contains_key("set-cookie"));
                    let body: serde_json::Value = response.json().await.expect("known result");
                    if original_operation == OPERATION {
                        assert_eq!(body["original"], serde_json::json!({"scope":"current","initiatorId":id}));
                        assert_eq!(body["count"], 1); assert_eq!(body["revocations"][0]["id"], id);
                        assert_eq!(chrono::DateTime::parse_from_rfc3339(body["revocations"][0]["revokedAt"].as_str().expect("time")).expect("timestamp").with_timezone(&chrono::Utc), revoked_at);
                    } else { assert_eq!(body, serde_json::json!({"code":"ok","original":null,"count":0,"revocations":[]})); }
                    let row = observer.client().query_one(query, &[&OPERATION]).await.expect("independent facts before replay");
                    assert_eq!(row.get::<_, String>("original"), original); assert!(row.get::<_, bool>("removed"));
                    proxy.release();
                    let replay = request(replay_operation).await.expect("normal original read");
                    assert_eq!(replay.status(), 200); assert!(!replay.headers().contains_key("set-cookie"));
                    assert_eq!(replay.json::<serde_json::Value>().await.expect("same results"), body);
                }
                stop.send(()).expect("stop");
                let (application, result) = task.await.expect("server task"); result.expect("drained"); drop(application);
                timeout(Duration::from_secs(3), async { loop {
                    if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND usename='clerk' AND pid<>pg_backend_pid())", &[]).await.expect("actual backends ended").get::<_, bool>(0) { break; }
                    tokio::task::yield_now().await;
                }}).await.expect("backend termination");
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
                assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err(), "no additional SMS through replay and drain");
                observer.terminate().await.expect("observer ended"); sms.stop().await; proxy.stop().await;
            }).await.expect("HTTP read cleanup budget");
        });
    });
}

#[test]
fn logout_recovery_keeps_known_outcomes_and_single_final_diagnostics() {
    use fixture::database_proxy::{Fault, Proxy};

    let cases = [
        (true, Fault::LostReadCommit, false),
        (true, Fault::ReadCommitTermination, false),
        (true, Fault::StalledReadCommit, false),
        (false, Fault::LostReadCommit, false),
        (false, Fault::ReadCommitTermination, false),
        (false, Fault::StalledReadCommit, false),
        (true, Fault::UnknownCommit, false),
        (true, Fault::StalledReadCommit, true),
    ];
    if std::env::var_os("NUCLEUS_TEST_LOGOUT_RECOVERY").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let child = tokio::process::Command::new(
                std::env::current_exe().expect("test executable"),
            )
            .args([
                "--exact",
                "revocation::logout_recovery_keeps_known_outcomes_and_single_final_diagnostics",
                "--nocapture",
            ])
            .env("NUCLEUS_TEST_LOGOUT_RECOVERY", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("isolated diagnostics process");
            let output = timeout(Duration::from_secs(150), child.wait_with_output())
                .await
                .expect("child budget")
                .expect("child result");
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
                cases.len(),
                "exactly one final failure per original operation: {text}"
            );
            for (index, (committed, fault, cancelled)) in cases.into_iter().enumerate() {
                let operation = format!("00000000-0000-4000-8000-{:012x}", index + 1);
                let matching: Vec<_> = failures
                    .iter()
                    .filter(|event| event["fields"]["operation_id"] == operation)
                    .collect();
                assert_eq!(matching.len(), 1, "one final event for {operation}: {text}");
                let event = matching[0];
                let unknown = matches!(fault, Fault::UnknownCommit);
                assert_eq!(event["fields"]["stage"], if committed && !unknown {"completion_recovery"} else {"command"});
                let span = event["spans"]
                    .as_array()
                    .expect("request spans")
                    .iter()
                    .find(|span| span["route"] == "/api/v1/sessions")
                    .expect("original HTTP context");
                let request_id = span["request_id"]
                    .as_str()
                    .expect("server request identity");
                Uuid::parse_str(request_id).expect("server request UUID");
                if committed && !unknown {
                    assert!(event["fields"]["kind"].is_null(), "known recovery reports its retained cause: {event}");
                } else {
                    assert_eq!(event["fields"]["kind"], if unknown {"unknown"} else {"unavailable"});
                }
                let error = event["fields"]["error"]
                    .as_str()
                    .expect("safe original cause");
                assert!(
                    error.contains("stage: Commit, kind: Query(connection closed)"),
                    "original query failure retained: {event}"
                );
                if unknown {
                    assert!(event["fields"]["outcome"].is_null());
                    assert!(
                        error.starts_with("Unknown(Inner {") && error.contains("(Begin, Query("),
                        "{event}"
                    );
                } else {
                    if committed {
                        assert_eq!(event["fields"]["outcome"], "committed");
                        assert!(error.starts_with("Inner {"), "{event}");
                    } else {
                        assert!(event["fields"]["outcome"].is_null());
                        assert!(error.starts_with("Unavailable(Inner {"), "{event}");
                    }
                    match fault {
                        Fault::LostReadCommit => {
                            assert!(error.contains("(Commit, Query(connection closed))"), "readback COMMIT failed: {event}");
                            assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(), 2, "both original and recovery driver failures remain attached: {event}");
                        }
                        Fault::ReadCommitTermination => {
                            assert!(!error.contains("(Commit, "), "readback COMMIT was acknowledged: {event}");
                            assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(), 2, "actual original and recovery driver failures: {event}");
                        }
                        Fault::StalledReadCommit => {
                            assert!(error.contains("(Commit, Timeout(Elapsed("), "actual recovery deadline expired: {event}");
                            assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(), 1, "only the original driver failed; the stalled live recovery driver was awaited: {event}");
                        }
                        _ => unreachable!("selected readback faults"),
                    }
                }
                if !cancelled {
                    let response = events
                        .iter()
                        .find(|event| {
                            event["fields"]["stage"] == "http_response"
                                && event["spans"].as_array().is_some_and(|spans| {
                                    spans.iter().any(|span| span["request_id"] == request_id)
                                })
                        })
                        .expect("response associated with final work diagnostic");
                    assert_eq!(
                        response["fields"]["status"],
                        if committed && !unknown { 200 } else { 503 }
                    );
                }
            }
            for (index, secret) in [
                "13800000000",
                "13900000000",
                "__Host-nucleus-session",
                "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
                "fictional-sensitive-termination-detail",
                fixture::PASSWORD,
                "SELECT ",
                "INSERT ",
                "DELETE FROM ",
            ]
            .into_iter()
            .enumerate()
            {
                assert!(
                    !events.iter().any(|event| contains_text(event, secret)),
                    "private material in diagnostics at forbidden index {index}"
                );
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("current-thread driver observation").block_on(async {
            timeout(Duration::from_secs(130), async {
                for (index, (committed, fault, cancelled)) in cases.into_iter().enumerate() {
                    let unknown = matches!(fault, Fault::UnknownCommit);
                    let name = database(&server).await;
                    let supplier = Supplier::echoing().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let proxy = Proxy::start(&server).await;
                    let application = Application::start(
                        proxy_connections(&name, &proxy), keys(0),
                        services(8).expect("settings"),
                        process::Settings::new(Duration::from_secs(20)).expect("drain budget"),
                        supplier.client(), message_at(sms.address(), sms.authority()),
                    ).await.expect("application");
                    let issuance = application.registration().clone();
                    let (base, stop, mut server_task) = running(application).await;
                    let (id, cookie) = issued_session(&base, &mut sms).await;
                    age_issuance(&server, &name, &issuance).await;
                    let (member, _) = issued_session(&base, &mut sms).await;
                    let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
                    let before: Vec<i32> = observer.client().query_one("SELECT array_agg(pid ORDER BY pid) FROM pg_catalog.pg_stat_activity WHERE datname=current_database()", &[]).await.expect("original backends").get(0);
                    let operation = format!("00000000-0000-4000-8000-{:012x}", index + 1);
                    proxy.arm(if unknown { Fault::UnknownCommit } else if committed { Fault::DelayedCommit } else { Fault::DelayedRollback });
                    let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(25)).build().expect("HTTP waiter");
                    let mut caller = tokio::spawn(client.delete(format!("{base}/api/v1/sessions"))
                        .header("operation-id", &operation).header("session-id", &id)
                        .header("cookie", &cookie)
                        .header("origin", "https://example.com").header("x-real-ip", "203.0.113.70").send());
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original COMMIT boundary");
                    if unknown {
                        let response = (&mut caller).await.expect("caller").expect("unknown response");
                        assert_eq!(response.status(), 503);
                        assert!(!response.headers().contains_key("set-cookie"));
                        assert_eq!(response.json::<serde_json::Value>().await.expect("unknown body"), serde_json::json!({"code":"outcome_unknown"}));
                        proxy.release();
                    }
                    let (other, other_cookie) = issued_session_from(&base, &mut sms, "13900000000", "198.51.100.91").await;
                    if !unknown {
                        assert!(!caller.is_finished(), "later login completes while the original logout is still held");
                        if committed {
                            timeout(Duration::from_secs(3), async {
                                loop {
                                    let waiting: bool = observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND NOT(pid=ANY($1)) AND wait_event_type='Lock' AND query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock(%')", &[&before]).await.expect("actual recovery lock wait").get(0);
                                    if waiting { break; }
                                    tokio::task::yield_now().await;
                                }
                            }).await.expect("recovery waits for the original backend");
                        }
                        proxy.arm(fault);
                        proxy.release();
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("readback COMMIT fault");
                    }
                    let row = timeout(Duration::from_secs(3), async {
                        loop {
                            let row = observer.client().query_one("SELECT (SELECT count(*) FROM identity.revocations WHERE operation_id=$3::text::uuid AND id IN($1::text::uuid,$4::text::uuid)) AS count, (SELECT revoked_at FROM identity.revocations WHERE id=$1::text::uuid AND operation_id=$3::text::uuid) AS revoked_at, EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$1::text::uuid) AS retained, EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$4::text::uuid) AS member_retained, EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$2::text::uuid) AS other", &[&id, &other, &operation, &member]).await.expect("independent original facts before replay or drain");
                            if row.get::<_, i64>("count") == if committed {2}else{0} { break row; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("original durable outcome");
                    assert_eq!(row.get::<_, bool>("retained"), !committed);
                    assert_eq!(row.get::<_, bool>("member_retained"), !committed);
                    assert!(row.get::<_, bool>("other"));
                    let response = http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id", OTHER).header("session-id", &other).header("cookie", &other_cookie).send().await.expect("other account remains usable");
                    assert_eq!(response.status(), 200);
                    assert!(!response.headers().contains_key("set-cookie"));
                    if cancelled {
                        caller.abort();
                        assert!((&mut caller).await.expect_err("HTTP waiter cancelled").is_cancelled());
                        stop.send(()).expect("stop with readback still waiting");
                        assert!(timeout(Duration::from_millis(30), &mut server_task).await.is_err(), "accepted recovery keeps shutdown waiting");
                    } else {
                        if !unknown {
                            let response = timeout(Duration::from_secs(13), &mut caller).await.expect("same recovery deadline").expect("caller").expect("logout response");
                            if committed {
                                assert_eq!(response.status(), 200);
                                assert_eq!(response.headers()["cache-control"], "no-store");
                                let clear = cookie::Cookie::parse(response.headers()["set-cookie"].to_str().expect("clear header").to_owned()).expect("clear cookie");
                                assert_eq!(clear.name(), format!("__Host-nucleus-session-{id}"));
                                assert_ne!(clear.name(), format!("__Host-nucleus-session-{other}"));
                                assert_eq!(clear.value(), ""); assert_eq!(clear.path(), Some("/"));
                                assert_eq!(clear.secure(), Some(true)); assert_eq!(clear.http_only(), Some(true));
                                assert_eq!(clear.same_site(), Some(cookie::SameSite::Lax));
                                assert!(clear.domain().is_none() && clear.max_age().is_none());
                                assert_eq!(clear.expires_datetime(), Some(cookie::time::OffsetDateTime::UNIX_EPOCH));
                                let body = response.json::<serde_json::Value>().await.expect("completion");
                                assert_eq!(body["completion"]["id"], operation); assert_eq!(body["completion"]["count"], 2);
                                assert_eq!(chrono::DateTime::parse_from_rfc3339(body["completion"]["revokedAt"].as_str().expect("timestamp")).expect("timestamp").with_timezone(&chrono::Utc), row.get::<_, Option<chrono::DateTime<chrono::Utc>>>("revoked_at").expect("original revoked_at"));
                                let response = http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id", OTHER).header("session-id", &other).header("cookie", &other_cookie).send().await.expect("later account remains usable after the logout response");
                                assert_eq!(response.status(), 200);
                                assert!(!response.headers().contains_key("set-cookie"));
                            } else {
                                assert_eq!(response.status(), 503);
                                assert!(!response.headers().contains_key("set-cookie"));
                                assert_eq!(response.json::<serde_json::Value>().await.expect("uncommitted body"), serde_json::json!({"code":"dependency_unavailable"}));
                            }
                        }
                        stop.send(()).expect("stop completed work");
                    }
                    let (application, result) = timeout(Duration::from_secs(15), server_task).await.expect("bounded drain").expect("server task");
                    result.expect("drained"); drop(application);
                    proxy.release();
                    timeout(Duration::from_secs(3), async {
                        loop {
                            let ended: bool = observer.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename='clerk' AND pid<>pg_backend_pid())", &[]).await.expect("original and recovery backends ended").get(0);
                            if ended { break; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("driver termination does not substitute for backend observation");
                    assert_eq!(supplier.calls.load(Ordering::SeqCst), 3);
                    assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err(), "no further SMS through recovery and drain");
                    observer.terminate().await.expect("observer ended");
                    sms.stop().await; proxy.stop().await;
                }
            }).await.expect("all HTTP recovery cases");
        });
    });
}
