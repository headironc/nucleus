use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, role::*, supplier::*};

#[test]
fn correction_history_preserves_authorized_disclosures_and_safe_failure_reporting() {
    use fixture::database_proxy::{Fault, Proxy};

    const OPERATION: &str = "00000000-0000-4000-9000-000000000102";
    const MANAGEMENT_OPERATION: &str = "00000000-0000-4000-9000-000000000103";
    if std::env::var_os("NUCLEUS_TEST_CORRECTION_HISTORY").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let expectations = tempfile::NamedTempFile::new().expect("private expectations");
            let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "correction::correction_history_preserves_authorized_disclosures_and_safe_failure_reporting", "--nocapture"])
                .env("NUCLEUS_TEST_CORRECTION_HISTORY", "1")
                .env("NUCLEUS_TEST_CORRECTION_HISTORY_EXPECTATIONS", expectations.path())
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true)
                .spawn().expect("isolated diagnostics child");
            let output = timeout(Duration::from_secs(60), child.wait_with_output()).await.expect("child budget").expect("child completion");
            let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
            assert!(output.status.success(), "{text}");
            let events: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).expect("JSON event")).collect();
            let failures: Vec<_> = events.iter().filter(|event| event["level"] == "ERROR").collect();
            assert_eq!(failures.len(), 2, "one final failure for each deliberately stalled read: {text}");
            for (route, operation) in [("/api/v1/accounts/current/corrections", OPERATION), ("/api/v1/persons/{id}/corrections", MANAGEMENT_OPERATION)] {
            let event = failures.iter().find(|event| event["spans"].as_array().is_some_and(|spans| spans.iter().any(|span| span["route"] == route))).expect("one failure for this history route");
            assert_eq!(event["fields"]["stage"], "correction_history");
            assert_eq!(event["fields"]["kind"], "unavailable");
            assert!(event["fields"]["error"].as_str().expect("structured cause").contains("Timeout("));
            let span = event["spans"].as_array().expect("request spans").iter().find(|span| span["route"] == route).expect("history request");
            assert_eq!(span["method"], "GET"); assert_eq!(span["operation_id"], operation);
            let request_id = span["request_id"].as_str().expect("request identity"); Uuid::parse_str(request_id).expect("request UUID");
            let responses: Vec<_> = events.iter().filter(|event| event["fields"]["stage"] == "http_response" && event["spans"].as_array().is_some_and(|spans| spans.iter().any(|span| span["request_id"] == request_id))).collect();
            assert_eq!(responses.len(), 1); assert_eq!(responses[0]["fields"]["status"], 503);
            }
            let tokens: Vec<String> = serde_json::from_slice(&std::fs::read(expectations.path()).expect("private token expectations")).expect("tokens");
            for secret in tokens.iter().map(String::as_str).chain(["13800000000", "13900000000", "Fixture Registered", "Fixture Operator", "Alpha Name", "Beta Name", "00000020000229007X", "000000200002290029", "__Host-nucleus-session", "203.0.129.", fixture::PASSWORD, "SELECT ", "INSERT ", "UPDATE "]) {
                assert!(!events.iter().any(|event| contains_text(event, secret)), "private material in diagnostics");
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(50), async {
                let name = database(&server).await;
                let supplier = Supplier::echoing().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let application = Application::start(proxy_connections(&name, &proxy), keys(0), services(8).expect("settings"), process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
                let (base, stop, task) = running(application).await;
                let manager = issued_session_for(&base, &mut sms, "13900000000").await;
                let target = issued_session_for(&base, &mut sms, "13800000000").await;
                let client = http_client();
                let bare = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(10)).build().expect("no default origin");
                let path = "/api/v1/accounts/current/corrections";
                let read = |query: &str, identity: &(String, String), source: u8| client.get(format!("{base}{path}{query}"))
                    .header("operation-id", Uuid::new_v4().to_string()).header("session-id", &identity.0).header("cookie", &identity.1).header("x-real-ip", format!("203.0.129.{source}"));
                assert_eq!(read("", &target, 1).send().await.expect("initial self history").json::<serde_json::Value>().await.expect("empty page"), serde_json::json!({"code":"ok","count":0,"corrections":[]}));
                for (index, corrected_name, id_number) in [(0, "Alpha Name", "000000200002290029"), (1, "Beta Name", "00000020000229007X")] {
                    let candidate = correction_candidate(&base, &manager, "13800000000").await;
                    let response = client.post(format!("{base}/api/v1/corrections")).header("operation-id", Uuid::new_v4().to_string())
                        .header("session-id", &manager.0).header("cookie", &manager.1).header("x-real-ip", format!("203.0.129.{}", index + 2))
                        .json(&serde_json::json!({"target":candidate["candidate"]["target"],"name":corrected_name,"idNumber":id_number})).send().await.expect("real HTTP correction");
                    assert_eq!(response.status(), 201);
                }
                let observer = connections(&server, &name, "clerk").recover().await.expect("independent SQL");
                let person_id: Uuid = observer.client().query_one("SELECT account.person_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id WHERE session.id=$1::text::uuid", &[&target.0]).await.expect("original session person").get(0);
                let rows = observer.client().query("SELECT correction.id,correction.corrected_at,operator.person_id AS operator_id FROM identity.corrections correction JOIN identity.revisions operator ON operator.id=correction.operator_id WHERE correction.person_id=$1 ORDER BY correction.sequence DESC,correction.id DESC", &[&person_id]).await.expect("persistent corrections and original operator before list");
                assert_eq!(rows.len(), 2);
                let records: Vec<_> = rows.iter().zip([("Alpha Name", "0029", "Beta Name", "007X"), ("Fixture Registered", "007X", "Alpha Name", "0029")]).map(|(row, (before_name, before_number, after_name, after_number))| serde_json::json!({
                    "id":row.get::<_,Uuid>("id"),"before":{"name":before_name,"idNumber":{"suffix":before_number}},
                    "after":{"name":after_name,"idNumber":{"suffix":after_number}},"correctedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("corrected_at")
                })).collect();
                let managed: Vec<_> = rows.iter().zip([("Alpha Name", "000000200002290029", "Beta Name", "00000020000229007X"), ("Fixture Registered", "00000020000229007X", "Alpha Name", "000000200002290029")]).map(|(row, (before_name, before_number, after_name, after_number))| serde_json::json!({
                    "id":row.get::<_,Uuid>("id"),"before":{"name":before_name,"idNumber":before_number},
                    "after":{"name":after_name,"idNumber":after_number},"operator":{"personId":row.get::<_,Uuid>("operator_id"),"name":"Fixture Operator"},
                    "correctedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("corrected_at")
                })).collect();
                let management_path = format!("/api/v1/persons/{person_id}/corrections");
                let management_read = |query: &str, identity: &(String, String), source: u8| client.get(format!("{base}{management_path}{query}"))
                    .header("operation-id", Uuid::new_v4().to_string()).header("session-id", &identity.0).header("cookie", &identity.1).header("x-real-ip", format!("203.0.129.{source}"));
                let snapshot = "SELECT jsonb_build_object('person',(SELECT to_jsonb(person) FROM identity.persons person WHERE id=$1),'corrections',(SELECT jsonb_agg(to_jsonb(correction) ORDER BY sequence) FROM identity.corrections correction WHERE person_id=$1),'revisions',(SELECT jsonb_agg(to_jsonb(revision) ORDER BY id) FROM identity.revisions revision WHERE person_id=$1),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session WHERE account_id=(SELECT id FROM identity.accounts WHERE person_id=$1)))::text";
                let original: String = observer.client().query_one(snapshot, &[&person_id]).await.expect("facts before all reads").get(0);
                for (query, expected) in [("", records.clone()), ("?page=1&size=1", vec![records[0].clone()]), ("?page=2&size=1", vec![records[1].clone()]), ("?page=3&size=1", vec![]), ("?size=100", records.clone())] {
                    let response = read(query, &target, 4).send().await.expect("self page without management permission");
                    assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("exact disclosure"), serde_json::json!({"code":"ok","count":2,"corrections":expected}));
                }
                assert_eq!(read("", &manager, 5).send().await.expect("manager's own history").json::<serde_json::Value>().await.expect("other person has no records"), serde_json::json!({"code":"ok","count":0,"corrections":[]}));
                for (query, expected) in [("", managed.clone()), ("?page=1&size=1", vec![managed[0].clone()]), ("?page=2&size=1", vec![managed[1].clone()]), ("?page=3&size=1", vec![]), ("?size=100", managed.clone())] {
                    let response = management_read(query, &manager, 20).send().await.expect("authorized full history");
                    assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("exact management disclosure"), serde_json::json!({"code":"ok","count":2,"corrections":expected}));
                }
                let operator_id: Uuid = rows[0].get("operator_id");
                for (builder, status, code) in [
                    (management_read("", &target, 21), 403, "permission_denied"),
                    (management_read("", &manager, 22).header("origin", "https://wrong.invalid"), 403, "permission_denied"),
                    (client.get(format!("{base}{management_path}?page=0")).header("operation-id", Uuid::new_v4().to_string()).header("x-real-ip", "203.0.129.23"), 401, "authentication_invalid"),
                    (client.get(format!("{base}/api/v1/persons/invalid/corrections?page=0")).header("operation-id", Uuid::new_v4().to_string()).header("x-real-ip", "203.0.129.24"), 400, "invalid_input"),
                    (client.get(format!("{base}/api/v1/persons/{operator_id}/corrections")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &manager.0).header("cookie", &manager.1).header("x-real-ip", "203.0.129.25"), 404, "target_unavailable"),
                    (client.get(format!("{base}/api/v1/persons/{}/corrections", Uuid::new_v4())).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &manager.0).header("cookie", &manager.1).header("x-real-ip", "203.0.129.26"), 404, "target_unavailable"),
                ] {
                    let response = builder.send().await.expect("management qualification or input refusal");
                    assert_eq!(response.status(), status); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe refusal"), serde_json::json!({"code":code}));
                }
                for query in ["?page=0", "?page=-1", "?size=0", "?size=101", "?size=", "?page=abc", "?page=1&page=2", "?size=1&size=2", "?personId=00000000-0000-4000-9000-000000000001", "?page=9223372036854775807&size=2", "?page=9223372036854775808"] {
                    let response = read(query, &target, 6).send().await.expect("invalid pagination");
                    assert_eq!(response.status(), 400); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe input failure"), serde_json::json!({"code":"invalid_input"}));
                    let response = management_read(query, &manager, 27).send().await.expect("management pagination has the same controlled boundary");
                    assert_eq!(response.status(), 400); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe management input failure"), serde_json::json!({"code":"invalid_input"}));
                }
                for (builder, status, code) in [
                    (client.get(format!("{base}{path}?page=0")).header("operation-id", Uuid::new_v4().to_string()).header("x-real-ip", "203.0.129.7"), 401, "authentication_invalid"),
                    (read("", &target, 8).header("authorization", "Bearer ignored"), 400, "invalid_input"),
                    (read("", &target, 9).header("origin", "https://wrong.invalid"), 403, "permission_denied"),
                    (read("", &target, 10).header("sec-fetch-site", "cross-site"), 403, "permission_denied"),
                    (bare.get(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &target.0).header("cookie", &target.1).header("x-real-ip", "203.0.129.11"), 403, "permission_denied"),
                ] {
                    let response = builder.send().await.expect("authentication and origin before list");
                    assert_eq!(response.status(), status); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe refusal"), serde_json::json!({"code":code}));
                }
                let response = bare.get(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &target.0).header("cookie", &target.1).header("sec-fetch-site", "same-origin").header("x-real-ip", "203.0.129.12").send().await.expect("same-origin without Origin");
                assert_eq!(response.status(), 200);
                let response = bare.get(format!("{base}{management_path}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &manager.0).header("cookie", &manager.1).header("sec-fetch-site", "same-origin").header("x-real-ip", "203.0.129.28").send().await.expect("management same-origin fallback");
                assert_eq!(response.status(), 200);
                for (query, status) in [("", 200), ("?page=0", 400)] {
                    let response = client.head(format!("{base}{path}{query}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &target.0).header("cookie", &target.1).header("x-real-ip", "203.0.129.13").send().await.expect("automatic HEAD");
                    assert_eq!(response.status(), status); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie")); assert!(response.bytes().await.expect("HEAD body").is_empty());
                    let response = client.head(format!("{base}{management_path}{query}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &manager.0).header("cookie", &manager.1).header("x-real-ip", "203.0.129.29").send().await.expect("management HEAD");
                    assert_eq!(response.status(), status); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie")); assert!(response.bytes().await.expect("management HEAD body").is_empty());
                }
                let response = client.post(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", "invalid").send().await.expect("unsupported method before identity");
                assert_eq!(response.status(), 405);
                let mut methods: Vec<_> = response.headers()["allow"].to_str().expect("Allow").split(',').map(str::trim).collect(); methods.sort_unstable(); assert_eq!(methods, ["GET", "HEAD"]);
                let response = client.post(format!("{base}{management_path}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", "invalid").send().await.expect("management unsupported method before identity");
                assert_eq!(response.status(), 405);
                let mut methods: Vec<_> = response.headers()["allow"].to_str().expect("Allow").split(',').map(str::trim).collect(); methods.sort_unstable(); assert_eq!(methods, ["GET", "HEAD"]);
                let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
                let route = &document["paths"][path];
                assert_eq!(route["get"]["operationId"], "listCurrentCorrections"); assert!(route["head"]["responses"]["200"].get("content").is_none());
                let parameters = route["get"]["parameters"].as_array().expect("request parameters"); assert_eq!(parameters.len(), 4);
                assert!(!parameters.iter().any(|parameter| parameter["name"] == "Origin" || parameter["name"] == "personId"));
                for name in ["Operation-Id", "Session-Id"] { assert!(parameters.iter().any(|parameter| parameter["name"] == name && parameter["required"] == true)); }
                for name in ["page", "size"] { let parameter = parameters.iter().find(|parameter| parameter["name"] == name).expect("pagination"); assert_eq!(parameter["required"], false); assert_eq!(parameter["schema"]["minimum"], 1); }
                assert_eq!(parameters.iter().find(|parameter| parameter["name"] == "size").expect("bounded size")["schema"]["maximum"], 100);
                let resolve = |schema: &serde_json::Value| {
                    let mut component = document.pointer(schema["$ref"].as_str().expect("schema reference").strip_prefix('#').expect("local reference")).expect("schema");
                    while let Some(reference) = component["$ref"].as_str() {
                        component = document.pointer(reference.strip_prefix('#').expect("local reference")).expect("owning value schema");
                    }
                    component
                };
                let envelope = resolve(&route["get"]["responses"]["200"]["content"]["application/json"]["schema"]);
                assert_eq!(envelope["required"], serde_json::json!(["code", "count", "corrections"]));
                let record = resolve(&envelope["properties"]["corrections"]["items"]);
                assert_eq!(record["required"], serde_json::json!(["id", "before", "after", "correctedAt"])); assert_eq!(record["properties"].as_object().expect("restricted correction").len(), 4);
                assert_eq!(resolve(&record["properties"]["id"])["format"], "uuid");
                for side in ["before", "after"] { let identity = resolve(&record["properties"][side]); assert_eq!(identity["required"], serde_json::json!(["name", "idNumber"])); let number = resolve(&identity["properties"]["idNumber"]); assert_eq!(number["required"], serde_json::json!(["suffix"])); assert_eq!(number["properties"].as_object().expect("only a suffix").len(), 1); }
                for method in ["get", "head"] { for status in ["200", "400", "401", "403", "429", "503"] { assert_eq!(route[method]["responses"][status]["headers"]["Cache-Control"]["description"], "no-store"); } assert!(route[method]["responses"]["429"]["headers"].get("Retry-After").is_some()); }
                let route = &document["paths"]["/api/v1/persons/{id}/corrections"];
                assert_eq!(route["get"]["operationId"], "listPersonCorrections"); assert!(route["head"]["responses"]["200"].get("content").is_none());
                let parameters = route["get"]["parameters"].as_array().expect("management parameters"); assert_eq!(parameters.len(), 5);
                assert!(!parameters.iter().any(|parameter| parameter["name"] == "Origin"));
                for name in ["id", "Operation-Id", "Session-Id"] { assert!(parameters.iter().any(|parameter| parameter["name"] == name && parameter["required"] == true)); }
                let envelope = resolve(&route["get"]["responses"]["200"]["content"]["application/json"]["schema"]);
                assert_eq!(envelope["required"], serde_json::json!(["code", "count", "corrections"]));
                let record = resolve(&envelope["properties"]["corrections"]["items"]);
                assert_eq!(record["required"], serde_json::json!(["id", "before", "after", "operator", "correctedAt"]));
                assert_eq!(record["properties"].as_object().expect("management correction fields").len(), 5);
                assert_eq!(resolve(&record["properties"]["id"])["format"], "uuid");
                for side in ["before", "after"] { let identity = resolve(&record["properties"][side]); assert_eq!(identity["required"], serde_json::json!(["name", "idNumber"])); assert_eq!(resolve(&identity["properties"]["idNumber"])["type"], "string"); }
                let operator = resolve(&record["properties"]["operator"]); assert_eq!(operator["required"], serde_json::json!(["personId", "name"])); assert_eq!(operator["properties"].as_object().expect("only authorized operator fields").len(), 2); assert_eq!(resolve(&operator["properties"]["personId"])["format"], "uuid");
                for method in ["get", "head"] { for status in ["200", "400", "401", "403", "404", "429", "503"] { assert_eq!(route[method]["responses"][status]["headers"]["Cache-Control"]["description"], "no-store"); } assert!(route[method]["responses"]["429"]["headers"].get("Retry-After").is_some()); }
                let mut limited = false;
                for index in 0..60 {
                    let endpoint = match index % 3 { 0 => path, 1 => management_path.as_str(), _ => "/api/v1/accounts/current" };
                    let response = client.get(format!("{base}{endpoint}")).header("operation-id", Uuid::new_v4().to_string()).header("x-real-ip", "203.0.129.250").send().await.expect("shared metadata budget");
                    if response.status() == 429 { assert!(response.headers()["retry-after"].to_str().expect("Retry-After").parse::<u64>().expect("seconds") > 0); assert_eq!(response.json::<serde_json::Value>().await.expect("rate limit"), serde_json::json!({"code":"rate_limited"})); limited = true; break; }
                    assert_eq!(response.status(), 401);
                }
                assert!(limited, "history shares the bounded metadata budget");
                proxy.arm(Fault::StalledCorrectionPage);
                let caller = tokio::spawn(client.get(format!("{base}{path}")).header("operation-id", OPERATION).header("session-id", &target.0).header("cookie", &target.1).header("x-real-ip", "203.0.129.14").timeout(Duration::from_secs(10)).send());
                let pid = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("history scan stopped after identity confirmation");
                let response = timeout(Duration::from_secs(6), caller).await.expect("read deadline").expect("caller").expect("failure response");
                assert_eq!(response.status(), 503); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie")); assert_eq!(response.json::<serde_json::Value>().await.expect("safe dependency failure"), serde_json::json!({"code":"dependency_unavailable"}));
                // Keep the history scan withheld until the timed-out backend actually ends.
                timeout(Duration::from_secs(3), async { loop { if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("backend lifetime").get::<_, bool>(0) { break; } tokio::task::yield_now().await; }}).await.expect("failed read backend ended");
                assert_eq!(observer.client().query_one(snapshot, &[&person_id]).await.expect("no read effects before retry").get::<_, String>(0), original);
                assert_eq!(read("", &target, 15).send().await.expect("fresh retry").json::<serde_json::Value>().await.expect("whole page after failure"), serde_json::json!({"code":"ok","count":2,"corrections":records}));
                proxy.arm(Fault::StalledCorrectionPage);
                let caller = tokio::spawn(client.get(format!("{base}{management_path}")).header("operation-id", MANAGEMENT_OPERATION).header("session-id", &manager.0).header("cookie", &manager.1).header("x-real-ip", "203.0.129.30").timeout(Duration::from_secs(10)).send());
                let pid = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("management count stopped after qualification");
                let response = timeout(Duration::from_secs(6), caller).await.expect("management read deadline").expect("caller").expect("management failure response");
                assert_eq!(response.status(), 503); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie")); assert_eq!(response.json::<serde_json::Value>().await.expect("safe management dependency failure"), serde_json::json!({"code":"dependency_unavailable"}));
                timeout(Duration::from_secs(3), async { loop { if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("management backend lifetime").get::<_, bool>(0) { break; } tokio::task::yield_now().await; }}).await.expect("failed management backend ended");
                assert_eq!(observer.client().query_one(snapshot, &[&person_id]).await.expect("management failure has no effects before retry").get::<_, String>(0), original);
                assert_eq!(management_read("", &manager, 31).send().await.expect("fresh management retry").json::<serde_json::Value>().await.expect("full management page after failure"), serde_json::json!({"code":"ok","count":2,"corrections":managed}));
                stop.send(()).expect("stop"); let (application, result) = task.await.expect("server"); result.expect("drained"); drop(application);
                assert_eq!(observer.client().query_one(snapshot, &[&person_id]).await.expect("reads and drain preserve facts").get::<_, String>(0), original);
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 2); assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err(), "no new supplier calls through failure, retry and drain");
                let tokens: Vec<_> = [&manager, &target].into_iter().map(|identity| identity.1.split_once('=').expect("cookie pair").1).collect();
                std::fs::write(std::env::var_os("NUCLEUS_TEST_CORRECTION_HISTORY_EXPECTATIONS").expect("private path"), serde_json::to_vec(&tokens).expect("token JSON")).expect("private expectations");
                observer.terminate().await.expect("observer ended"); sms.stop().await; proxy.stop().await;
            }).await.expect("self-history HTTP acceptance budget");
        });
    });
}

#[test]
fn corrections_confirm_complete_values_and_preserve_original_http_results() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(70),async {
            let name=database(&server).await;
            let supplier=Supplier::echoing().await;
            let mut sms=fixture::supplier::Supplier::start().await;
            let application=Application::start(connections(&server,&name,"clerk"),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("startup");
            let (base,stop,task)=running(application).await;
            let administrator=issued_session_for(&base,&mut sms,"13900000000").await;
            let (target,registration)=issued_session_and_registration(&base,&mut sms,"13800000000","198.51.100.90","00000020000229007X").await;
            let registration=registration.expect("this target was actually registered through R2");
            let manager=issued_session_with_identity(&base,&mut sms,"13700000000","198.51.100.92","000000200002290029").await;
            let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
            let subject=observer.client().query_one("SELECT account.id,account.person_id,person.revision_id,account.id_number,to_jsonb(revision)::text AS original_revision FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id JOIN identity.revisions revision ON revision.id=person.revision_id WHERE session.id=$1::text::uuid",&[&target.0]).await.expect("original independently bound target");
            let account_id:Uuid=subject.get("id");let person_id:Uuid=subject.get("person_id");let old_revision:Uuid=subject.get("revision_id");
            let operator_revision=uuid_of(&current_account(&base,&manager).await["revisionId"]);
            const UNCHANGED:&str="SELECT jsonb_build_object('account',to_jsonb(account)-'id_number'-'sequence'-'version'-'changed_at'-'seal'-'seal_key_id'-'seal_version','sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session WHERE account_id=account.id),'handoffs',(SELECT jsonb_agg(to_jsonb(handoff) ORDER BY handoff.id) FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=account.id),'receipt',(SELECT to_jsonb(receipt) FROM identity.receipts receipt WHERE account_id=account.id),'holder',(SELECT to_jsonb(holder) FROM access.holders holder WHERE id=account.id),'appointments',(SELECT jsonb_agg(to_jsonb(appointment) ORDER BY sequence) FROM access.appointments appointment WHERE person_id=account.person_id),'suspensions',(SELECT jsonb_agg(to_jsonb(suspension) ORDER BY sequence) FROM identity.suspensions suspension WHERE person_id=account.person_id),'resumptions',(SELECT jsonb_agg(to_jsonb(resumption) ORDER BY sequence) FROM identity.resumptions resumption WHERE person_id=account.person_id),'revocations',(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM identity.revocations revocation WHERE account_id=account.id))::text FROM identity.accounts account WHERE account.id=$1";
            let unchanged:String=observer.client().query_one(UNCHANGED,&[&account_id]).await.expect("original unrelated facts").get(0);
            let original_account:String=observer.client().query_one("SELECT to_jsonb(account)::text FROM identity.accounts account WHERE id=$1",&[&account_id]).await.expect("original Account state").get(0);
            let expected=serde_json::json!({"code":"ok","candidate":{"target":{"personId":person_id,"revisionId":old_revision},"name":"Fixture Registered","idNumber":"00000020000229007X"}});
            assert_eq!(correction_candidate(&base,&administrator,"13800000000").await,expected);
            let path=format!("/api/v1/corrections/{person_id}");
            let client=http_client();
            let read=|path:&str,identity:&(String,String)|client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&identity.0).header("cookie",&identity.1).header("x-real-ip","203.0.124.2");
            assert_eq!(read(&path,&administrator).send().await.expect("person candidate").json::<serde_json::Value>().await.expect("same exact candidate"),expected);
            let input=serde_json::json!({"target":expected["candidate"]["target"],"name":"  Corrected e\u{301}  ","idNumber":"00000020000229007x"});
            let operation=Uuid::new_v4();
            let request=|path:&str,identity:Option<&(String,String)>,operation:Uuid,source:u8|{
                let mut request=client.post(format!("{base}{path}")).header("operation-id",operation.to_string()).header("x-real-ip",format!("203.0.125.{source}"));
                if let Some(identity)=identity {request=request.header("session-id",&identity.0).header("cookie",&identity.1);}
                request
            };
            let bare=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client without implicit origin");
            let mut invalid=input.clone();invalid["name"]=serde_json::json!("\n");
            let mut invalid_number=input.clone();invalid_number["idNumber"]=serde_json::json!("000000200002290071");
            let mut unknown=input.clone();unknown["extra"]=serde_json::json!(true);
            let mut unknown_target=input.clone();unknown_target["target"]["accountId"]=serde_json::json!(account_id);
            let mut missing=input.clone();missing.as_object_mut().expect("input").remove("idNumber");
            let mut null=input.clone();null["target"]["revisionId"]=serde_json::Value::Null;
            let mut invalid_target=input.clone();invalid_target["target"]["personId"]=serde_json::json!("invalid-person");
            let mut cadence=tokio::time::interval_at(tokio::time::Instant::now()+Duration::from_millis(120),Duration::from_millis(120));
            for (builder,status,code) in [
                (client.post(format!("{base}/api/v1/corrections")).json(&input),400,"invalid_input"),
                (read("/api/v1/corrections/not-a-uuid",&administrator),400,"invalid_input"),
                (request("/api/v1/corrections",None,operation,1).json(&invalid),401,"authentication_invalid"),
                (request("/api/v1/corrections",None,operation,2).header("session-id",&administrator.0).json(&input),401,"authentication_invalid"),
                (request("/api/v1/corrections",None,operation,3).header("cookie",&administrator.1).json(&input),401,"authentication_invalid"),
                (bare.post(format!("{base}/api/v1/corrections")).header("operation-id",operation.to_string()).header("session-id",&administrator.0).header("cookie",&administrator.1).header("x-real-ip","203.0.125.4").header("sec-fetch-site","same-origin").json(&invalid),403,"permission_denied"),
                (request("/api/v1/corrections",Some(&administrator),operation,5).header("origin","https://wrong.invalid").json(&invalid),403,"permission_denied"),
                (request("/api/v1/corrections",Some(&administrator),operation,6).header("sec-fetch-site","cross-site").json(&input),403,"permission_denied"),
                (request("/api/v1/corrections",Some(&manager),operation,7).json(&invalid),422,"invalid_input"),
                (request("/api/v1/corrections",Some(&manager),operation,8).json(&input),403,"permission_denied"),
                (request("/api/v1/corrections",Some(&administrator),operation,9).json(&invalid_number),422,"invalid_input"),
                (request("/api/v1/corrections",Some(&administrator),operation,10).json(&unknown),422,"invalid_input"),
                (request("/api/v1/corrections",Some(&administrator),operation,11).json(&unknown_target),422,"invalid_input"),
                (request("/api/v1/corrections",Some(&administrator),operation,12).json(&missing),422,"invalid_input"),
                (request("/api/v1/corrections",Some(&administrator),operation,13).json(&null),422,"invalid_input"),
                (request("/api/v1/corrections",Some(&administrator),operation,14).json(&invalid_target),422,"invalid_input"),
                (request("/api/v1/corrections",Some(&administrator),operation,15).body("{}"),415,"invalid_input"),
                (request("/api/v1/corrections",Some(&administrator),operation,16).header("content-type","application/json").body("{"),400,"invalid_input"),
                (request("/api/v1/corrections",Some(&administrator),operation,17).header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
                (request("/api/v1/corrections/candidates",Some(&manager),operation,18).json(&serde_json::json!({"mobile":"138"})),422,"invalid_input"),
                (request("/api/v1/corrections/candidates",Some(&manager),operation,19).json(&serde_json::json!({"mobile":"13800000000"})),403,"permission_denied"),
                (read(&path,&manager),403,"permission_denied"),
            ] {
                cadence.tick().await;let response=builder.send().await.expect("ordered correction rejection");
                assert_eq!(response.status(),status);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe error"),serde_json::json!({"code":code}));
            }
            let mut roles=Vec::with_capacity(3);
            for (index,permission) in ["institution.roles","institution.accounts","institution.corrections"].into_iter().enumerate() {
                cadence.tick().await;
                let response=request("/api/v1/roles",Some(&administrator),Uuid::new_v4(),30).json(&serde_json::json!({"name":format!("Correction authority {index}"),"level":200,"permissions":[permission]})).send().await.expect("real narrow role");assert_eq!(response.status(),201);
                let role=response.json::<serde_json::Value>().await.expect("role")["role"].clone();
                replace_role(&base,&administrator,"13700000000",&role,u8::try_from(90+index).expect("source")).await;
                cadence.tick().await;
                let response=read(&path,&manager).send().await.expect("exact correction permission");assert_eq!(response.status(),if index==2 {200}else{403});
                if index==2 {assert_eq!(response.json::<serde_json::Value>().await.expect("complete candidate"),expected);}else{
                    cadence.tick().await;let response=request("/api/v1/corrections",Some(&manager),operation,40).json(&input).send().await.expect("adjacent permissions cannot correct");assert_eq!(response.status(),403);assert_eq!(response.json::<serde_json::Value>().await.expect("exact permission"),serde_json::json!({"code":"permission_denied"}));
                }
                roles.push(role);
            }
            let [maintenance_role,_,correction_role]:[serde_json::Value;3]=roles.try_into().expect("three genuinely created narrow roles");
            assert_eq!(correction_candidate(&base,&manager,"13800000000").await,expected,"correction permission needs no Workforce grant");
            let manager_person=uuid_of(&current_account(&base,&manager).await["personId"]);
            let admin_person=uuid_of(&current_account(&base,&administrator).await["personId"]);
            for person in [manager_person,admin_person,Uuid::new_v4()] {
                let response=read(&format!("/api/v1/corrections/{person}"),&manager).send().await.expect("self superior and absent hidden");assert_eq!(response.status(),404);assert_eq!(response.json::<serde_json::Value>().await.expect("hidden"),serde_json::json!({"code":"target_unavailable"}));
            }
            cadence.tick().await;
            let response=request("/api/v1/corrections",Some(&manager),operation,31).json(&input).send().await.expect("name-only correction");assert_eq!(response.status(),201);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.headers()["cache-control"],"no-store");
            let original=response.json::<serde_json::Value>().await.expect("new result");
            let row=observer.client().query_one("SELECT correction.id,correction.corrected_at,correction.person_id=$2 AND correction.operator_id=$3 AND correction.sequence=2 AND revision.person_id=correction.person_id AND revision.transaction_id=correction.transaction_id AND person.revision_id=correction.id AND person.sequence=2 AS bound FROM identity.corrections correction JOIN identity.revisions revision ON revision.id=correction.id JOIN identity.persons person ON person.id=correction.person_id WHERE correction.operation_id=$1",&[&operation,&person_id,&operator_revision]).await.expect("independent durable correction before replay");assert!(row.get::<_,bool>("bound"));
            let first_id:Uuid=row.get("id");let expected_original=serde_json::json!({"code":"ok","correction":{"id":first_id,"correctedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("corrected_at")}});assert_eq!(original,expected_original);
            assert_eq!(observer.client().query_one("SELECT id_number FROM identity.accounts WHERE id=$1",&[&account_id]).await.expect("name-only lookup").get::<_,Vec<u8>>(0),subject.get::<_,Vec<u8>>("id_number"));
            assert_eq!(observer.client().query_one(UNCHANGED,&[&account_id]).await.expect("unchanged business history and credentials").get::<_,String>(0),unchanged);
            assert_eq!(observer.client().query_one("SELECT to_jsonb(account)::text FROM identity.accounts account WHERE id=$1",&[&account_id]).await.expect("name-only correction retains the whole Account").get::<_,String>(0),original_account);
            assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.projections WHERE account_id=$1)",&[&account_id]).await.expect("name-only correction needs no Account projection").get::<_,bool>(0));
            assert_eq!(observer.client().query_one("SELECT to_jsonb(revision)::text FROM identity.revisions revision WHERE id=$1",&[&old_revision]).await.expect("immutable old version").get::<_,String>(0),subject.get::<_,String>("original_revision"));
            let corrected=correction_candidate(&base,&manager,"13800000000").await;
            assert_eq!(corrected,serde_json::json!({"code":"ok","candidate":{"target":{"personId":person_id,"revisionId":first_id},"name":"Corrected é","idNumber":"00000020000229007X"}}));
            let response=read("/api/v1/accounts/current",&target).send().await.expect("original session survives correction");assert_eq!(response.status(),200);assert!(!response.headers().contains_key("set-cookie"));
            let current=response.json::<serde_json::Value>().await.expect("self projection");assert!(current.to_string().contains("Corrected é"));assert!(!current.to_string().contains("00000020000229007X"));
            let mut normalized=input.clone();normalized["name"]=serde_json::json!("Corrected é");normalized["idNumber"]=serde_json::json!("00000020000229007X");
            let mut unchanged_input=normalized.clone();unchanged_input["target"]=corrected["candidate"]["target"].clone();
            let mut occupied=unchanged_input.clone();occupied["idNumber"]=serde_json::json!("000000200002290010");
            for (body,id,status,code) in [(&normalized,operation,200,"ok"),(&normalized,Uuid::new_v4(),409,"target_changed"),(&unchanged_input,Uuid::new_v4(),409,"state_conflict"),(&occupied,Uuid::new_v4(),409,"id_number_taken")] {
                cadence.tick().await;let response=request("/api/v1/corrections",Some(&manager),id,32).json(body).send().await.expect("replay or current confirmation");assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie"));let body=response.json::<serde_json::Value>().await.expect("result");assert_eq!(body["code"],code);if status==200 {assert_eq!(body,expected_original);}
            }
            let later_operation=Uuid::new_v4();let mut later=unchanged_input.clone();later["idNumber"]=serde_json::json!("000000200002290088");
            cadence.tick().await;let response=request("/api/v1/corrections",Some(&manager),later_operation,33).json(&later).send().await.expect("ID-number-only correction");assert_eq!(response.status(),201);let later_result=response.json::<serde_json::Value>().await.expect("second result");
            let later_row=observer.client().query_one("SELECT correction.id,correction.corrected_at,correction.person_id=$2 AND correction.operator_id=$3 AND correction.sequence=3 AND person.sequence=3 AND person.revision_id=correction.id AND revision.person_id=$2 AND revision.transaction_id=correction.transaction_id AND account.sequence=2 AND account.version=2 AND account.changed_at=correction.corrected_at AND projection.cause='correction' AND projection.person_id=person.id AND projection.sequence=account.sequence AND projection.operation_id=correction.operation_id AND projection.projected_at=account.changed_at AND projection.transaction_id=correction.transaction_id AS bound,account.id_number FROM identity.corrections correction JOIN identity.revisions revision ON revision.id=correction.id JOIN identity.persons person ON person.id=correction.person_id JOIN identity.accounts account ON account.person_id=person.id JOIN identity.projections projection ON projection.account_id=account.id AND projection.id=correction.id WHERE correction.operation_id=$1",&[&later_operation,&person_id,&operator_revision]).await.expect("second transaction before any replay");assert!(later_row.get::<_,bool>("bound"));let later_id:Uuid=later_row.get("id");assert_ne!(later_id,first_id);assert_ne!(later_row.get::<_,Vec<u8>>("id_number"),subject.get::<_,Vec<u8>>("id_number"));assert_eq!(later_result,serde_json::json!({"code":"ok","correction":{"id":later_id,"correctedAt":later_row.get::<_,chrono::DateTime<chrono::Utc>>("corrected_at")}}));
            assert_eq!(observer.client().query_one(UNCHANGED,&[&account_id]).await.expect("both corrections preserve other facts").get::<_,String>(0),unchanged);
            let response=client.post(format!("{base}/api/v1/registrations")).header("operation-id",registration.operation.to_string()).header("x-real-ip","203.0.124.7").json(&registration.input).send().await.expect("original R2 uses registered version");assert_eq!(response.status(),200);assert_eq!(response.headers()["set-cookie"].to_str().expect("cookie").split(';').next().expect("pair"),target.1);assert_eq!(response.json::<serde_json::Value>().await.expect("original session")["session"]["id"],target.0);
            cadence.tick().await;replace_role(&base,&administrator,"13800000000",&correction_role,94).await;
            assert_eq!(read(&path,&manager).send().await.expect("equal level current target hidden").status(),404);
            cadence.tick().await;let response=request("/api/v1/corrections",Some(&manager),operation,34).json(&normalized).send().await.expect("original result before later target level and version");assert_eq!(response.status(),200);assert_eq!(response.json::<serde_json::Value>().await.expect("original"),expected_original);
            for (field,value) in [("name",serde_json::json!("Different")),("idNumber",serde_json::json!("000000200002290088")),("target",serde_json::json!({"personId":person_id,"revisionId":later_id}))] {
                let mut mismatch=normalized.clone();mismatch[field]=value;cadence.tick().await;let response=request("/api/v1/corrections",Some(&manager),operation,35).json(&mismatch).send().await.expect("permanent normalized request binding");assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch"),serde_json::json!({"code":"request_mismatch"}));
            }
            cadence.tick().await;let response=request("/api/v1/corrections",Some(&administrator),operation,36).json(&normalized).send().await.expect("original operator Person binding");assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("operator mismatch"),serde_json::json!({"code":"request_mismatch"}));
            correction_protocol(&base,&administrator,person_id).await;
            cadence.tick().await;
            let cancellation_operation=Uuid::new_v4();
            let response=request("/api/v1/accounts/current/cancellations",Some(&target),cancellation_operation,96).header("origin","https://example.com").json(&serde_json::json!({"accountId":account_id,"personId":person_id,"revisionId":later_id})).send().await.expect("real self cancellation after correction and role replacement");
            assert_eq!(response.status(),200);
            let cancellation=response.json::<serde_json::Value>().await.expect("original cancellation");
            let cancellation_id=uuid_of(&cancellation["cancellation"]["id"]);
            assert!(observer.client().query_one("SELECT cancellation.account_id=$2 AND cancellation.person_id=$3 AND cancellation.revision_id=$4 AND cancellation.operator_id=$4 AND cancellation.operation_id=$5 AND cancellation.sequence=4 AND person.sequence=4 AND holder.person_id=$3 AND holder.cancellation_id=cancellation.id AND holder.sequence=3 AND holder.version=3 AND projection.holder_id=holder.id AND projection.person_id=$3 AND projection.role_id=holder.role_id AND projection.assignment_id=holder.assignment_id AND projection.sequence=holder.sequence AND projection.projected_at=cancellation.cancelled_at AND projection.operation_id=cancellation.operation_id AND projection.transaction_id=cancellation.transaction_id AND EXISTS(SELECT 1 FROM identity.accounts account JOIN identity.projections projected ON projected.account_id=account.id AND projected.id=account.cancellation_id WHERE account.id=$2 AND account.person_id=$3 AND account.cancellation_id=cancellation.id AND account.mobile IS NULL AND account.id_number IS NULL AND account.suspension_id IS NULL AND account.sequence=3 AND account.version=3 AND account.changed_at=cancellation.cancelled_at AND projected.cause='cancellation' AND projected.sequence=account.sequence AND projected.projected_at=account.changed_at AND projected.operation_id=cancellation.operation_id AND projected.transaction_id=cancellation.transaction_id) AND NOT EXISTS(SELECT 1 FROM identity.sessions WHERE account_id=$2) AND NOT EXISTS(SELECT 1 FROM identity.receipts WHERE account_id=$2) AND NOT EXISTS(SELECT 1 FROM identity.avatars WHERE account_id=$2 AND released_at IS NULL) FROM identity.cancellations cancellation JOIN identity.persons person ON person.id=cancellation.person_id JOIN access.holders holder ON holder.id=cancellation.account_id JOIN access.projections projection ON projection.id=cancellation.id WHERE cancellation.id=$1",&[&cancellation_id,&account_id,&person_id,&later_id,&cancellation_operation]).await.expect("independent complete cancellation and retained Holder tombstone before correction").get::<_,bool>(0));
            let response=read(&path,&manager).send().await.expect("no-account person has no current level");assert_eq!(response.status(),200);let candidate=response.json::<serde_json::Value>().await.expect("full permanent person");assert_eq!(candidate,serde_json::json!({"code":"ok","candidate":{"target":{"personId":person_id,"revisionId":later_id},"name":"Corrected é","idNumber":"000000200002290088"}}));
            cadence.tick().await;let response=request("/api/v1/corrections/candidates",Some(&manager),Uuid::new_v4(),37).json(&serde_json::json!({"mobile":"13800000000"})).send().await.expect("mobile cannot locate absent account");assert_eq!(response.status(),404);
            let no_account=serde_json::json!({"target":candidate["candidate"]["target"],"name":"Permanent Person","idNumber":"000000200002290088"});let no_account_operation=Uuid::new_v4();
            cadence.tick().await;let response=request("/api/v1/corrections",Some(&manager),no_account_operation,38).json(&no_account).send().await.expect("correct permanent person without opening account");assert_eq!(response.status(),201);let result=response.json::<serde_json::Value>().await.expect("no-account result");
            let row=observer.client().query_one("SELECT correction.id,correction.corrected_at,correction.sequence=5 AND person.sequence=5 AND person.revision_id=correction.id AND correction.operator_id=$3 AND correction.appointment_id IS NULL AND EXISTS(SELECT 1 FROM identity.accounts account WHERE account.id=$4 AND account.person_id=$2 AND account.cancellation_id=$5 AND account.sequence=3 AND account.version=3 AND account.mobile IS NULL AND account.id_number IS NULL AND account.suspension_id IS NULL) AND (SELECT count(*) FROM identity.projections WHERE account_id=$4)=2 AND EXISTS(SELECT 1 FROM access.holders holder JOIN access.projections projection ON projection.id=holder.cancellation_id AND projection.holder_id=holder.id WHERE holder.id=$4 AND holder.person_id=$2 AND holder.cancellation_id=$5 AND holder.sequence=3 AND projection.sequence=holder.sequence AND projection.assignment_id=holder.assignment_id) AND (SELECT count(*) FROM access.appointments WHERE person_id=$2)=2 AS bound FROM identity.corrections correction JOIN identity.persons person ON person.id=correction.person_id WHERE correction.operation_id=$1 AND correction.person_id=$2",&[&no_account_operation,&person_id,&operator_revision,&account_id,&cancellation_id]).await.expect("no-account correction preserves the cancellation and Holder tombstone");assert!(row.get::<_,bool>("bound"));assert_eq!(result,serde_json::json!({"code":"ok","correction":{"id":row.get::<_,Uuid>("id"),"correctedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("corrected_at")}}));
            cadence.tick().await;let response=request("/api/v1/corrections",Some(&manager),operation,39).json(&normalized).send().await.expect("original completion survives loss of target account");assert_eq!(response.status(),200);assert_eq!(response.json::<serde_json::Value>().await.expect("same original"),expected_original);
            cadence.tick().await;replace_role(&base,&administrator,"13700000000",&maintenance_role,95).await;
            cadence.tick().await;let response=request("/api/v1/corrections",Some(&manager),operation,41).json(&normalized).send().await.expect("original operation does not restore lost basic permission");assert_eq!(response.status(),403);assert_eq!(response.json::<serde_json::Value>().await.expect("permission denied"),serde_json::json!({"code":"permission_denied"}));
            stop.send(()).expect("stop");let(application,result)=task.await.expect("server");result.expect("drained");drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),3);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"corrections and all replays never call a supplier");observer.terminate().await.expect("observer ended");sms.stop().await;
        }).await.expect("correction protocol and lifecycle budget");
    });
}

#[test]
fn correction_recovers_real_http_work_and_reports_once_after_cancellation() {
    use fixture::database_proxy::{Fault, Proxy};

    struct Coordination(Uuid);
    impl platform::cryptography::Input<2> for Coordination {
        fn fields(&self) -> [&[u8]; 2] {
            [b"identity/correction/operation", self.0.as_bytes()]
        }
    }
    if std::env::var_os("NUCLEUS_TEST_CORRECTION_RECOVERY").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let expectations=tempfile::NamedTempFile::new().expect("private expected credentials");
            let child=tokio::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact","correction::correction_recovers_real_http_work_and_reports_once_after_cancellation","--nocapture"])
                .env("NUCLEUS_TEST_CORRECTION_RECOVERY","1").env("NUCLEUS_TEST_CORRECTION_EXPECTATIONS",expectations.path())
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().expect("isolated diagnostics child");
            let output=timeout(Duration::from_secs(190),child.wait_with_output()).await.expect("child budget").expect("child result");
            let text=String::from_utf8(output.stderr).expect("JSON diagnostics");assert!(output.status.success(),"{text}");
            let events:Vec<serde_json::Value>=text.lines().map(|line|serde_json::from_str(line).expect("JSON event")).collect();
            let failures:Vec<_>=events.iter().filter(|event|event["level"]=="ERROR").collect();
            assert_eq!(failures.len(),4,"clean recovered commit succeeds; each remaining technical conclusion has one final event: {text}");
            for index in 0..5 {
                let operation=format!("00000000-0000-4000-a000-{:012x}",index+1);
                let matching:Vec<_>=failures.iter().filter(|event|event["fields"]["operation_id"]==operation).collect();
                if index==0 {assert!(matching.is_empty(),"clean recovery needs no failure event");continue;}
                assert_eq!(matching.len(),1,"one final event for original operation {operation}");
                let event=matching[0];
                assert_eq!(event["fields"]["stage"],if matches!(index,1|4) {"completion_recovery"} else {"command"});
                if matches!(index,1|4) {
                    assert!(event["fields"]["kind"].is_null(),"known recovery reports its retained cause: {event}");
                } else {
                    assert_eq!(event["fields"]["kind"],if index==3 {"unknown"}else{"unavailable"});
                }
                let spans=event["spans"].as_array().expect("original spans");
                assert!(spans.iter().any(|span|span["family"]=="Kernel(Correction)"&&span["operation_id"]==operation),"exact original action: {event}");
                let span=spans.iter().find(|span|span["route"]=="/api/v1/corrections").expect("original correction request span");assert_eq!(span["method"],"POST");assert_eq!(span["operation_id"],operation);
                let request_id=span["request_id"].as_str().expect("request ID");Uuid::parse_str(request_id).expect("UUID");
                let error=event["fields"]["error"].as_str().expect("safe original cause");
                assert!(error.contains("stage: Commit, kind: Query(connection closed)"),"original write commit failure retained: {event}");
                match index {
                    1=>{
                        assert_eq!(event["fields"]["outcome"],"committed");
                        assert!(error.starts_with("Inner {"),"{event}");
                        assert!(!error.contains("(Commit, "),"recovery COMMIT was acknowledged: {event}");
                        assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(),2,"original and recovery driver failures remain attached: {event}");
                    }
                    2=>{
                        assert!(event["fields"]["outcome"].is_null());
                        assert!(error.starts_with("Unavailable(Inner {")&&!error.contains("(Commit, ")&&!error.contains("(Begin, "),"known rollback keeps its original cause: {event}");
                        assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(),1,"recovery ended cleanly after the original driver failure: {event}");
                    }
                    3=>{
                        assert!(event["fields"]["outcome"].is_null());
                        assert!(error.starts_with("Unknown(Inner {")&&error.contains("(Begin, Query("),"failed required evidence stays unknown: {event}");
                    }
                    4=>{
                        assert_eq!(event["fields"]["outcome"],"committed");
                        assert!(error.starts_with("Inner {"),"{event}");
                        assert!(error.contains("(Commit, Query(connection closed))"),"recovery COMMIT receipt failed: {event}");
                        assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(),2,"both driver failures remain attached after waiter cancellation: {event}");
                    }
                    _=>unreachable!("selected cases"),
                }
                if index!=4 {
                    let response=events.iter().find(|event|event["fields"]["stage"]=="http_response"&&event["spans"].as_array().is_some_and(|spans|spans.iter().any(|span|span["request_id"]==request_id))).expect("response bound to the same original request");assert_eq!(response["fields"]["status"],if index==1 {201}else{503});
                }
            }
            let tokens:Vec<String>=serde_json::from_slice(&std::fs::read(expectations.path()).expect("expected private credentials")).expect("tokens");
            for secret in tokens.iter().map(String::as_str).chain(["13800000000","13900000000","Fixture Registered","Fixture Operator","Recovered Private Name","Different Private Name","00000020000229007X","000000200002290010","__Host-nucleus-session","AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","fictional-sensitive-termination-detail","203.0.127.1",fixture::PASSWORD,"SELECT ","INSERT ","UPDATE "]) {
                assert!(!events.iter().any(|event|contains_text(event,secret)),"private material in diagnostic strings");
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("actual driver error observation").block_on(async {
            timeout(Duration::from_secs(180),async {
                let mut tokens=Vec::new();
                for index in 0..5 {
                    timeout(Duration::from_secs(35),async {
                    let name=database(&server).await;let supplier=Supplier::echoing().await;let mut sms=fixture::supplier::Supplier::start().await;let proxy=Proxy::start(&server).await;
                    let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(15)).expect("drain"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("startup");
                    let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");let(base,stop,mut task)=running(application).await;
                    timeout(Duration::from_secs(3),async {loop {
                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND usename='clerk' AND state='idle' AND query LIKE 'SELECT family, operation_id, action, target, input, subject, person_id, revision_id, created_at, expires_at, seal, seal_key_id, seal_version FROM workforce.completions WHERE expires_at <= %')",&[]).await.expect("initial empty retention scan").get::<_,bool>(0) {break;}tokio::task::yield_now().await;
                    }}).await.expect("background scan finishes before fault selection");
                    let manager=issued_session_for(&base,&mut sms,"13900000000").await;let target=issued_session_for(&base,&mut sms,"13800000000").await;
                    for identity in [&manager,&target] {tokens.push(identity.1.split_once('=').expect("cookie pair").1.to_owned());}
                    let subject=observer.client().query_one("SELECT account.id,account.person_id,person.revision_id,account.id_number FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1::text::uuid",&[&target.0]).await.expect("independent target binding");
                    let account_id:Uuid=subject.get("id");let person_id:Uuid=subject.get("person_id");let revision_id:Uuid=subject.get("revision_id");
                    let operator_revision:Uuid=observer.client().query_one("SELECT person.revision_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1::text::uuid",&[&manager.0]).await.expect("original operator").get(0);
                    const EFFECTS:&str="SELECT jsonb_build_object('account',to_jsonb(account),'session',(SELECT to_jsonb(session) FROM identity.sessions session WHERE id=$2::text::uuid),'handoff',(SELECT to_jsonb(handoff) FROM identity.handoffs handoff WHERE id=$2::text::uuid),'receipt',(SELECT to_jsonb(receipt) FROM identity.receipts receipt WHERE account_id=$1),'holder',(SELECT to_jsonb(holder) FROM access.holders holder WHERE id=$1),'revocations',(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM identity.revocations revocation WHERE account_id=$1),'originalRevision',(SELECT to_jsonb(revision) FROM identity.revisions revision WHERE id=$3))::text FROM identity.accounts account WHERE id=$1";
                    let effects:String=observer.client().query_one(EFFECTS,&[&account_id,&target.0,&revision_id]).await.expect("untouched original state").get(0);
                    let candidate=correction_candidate(&base,&manager,"13800000000").await;assert_eq!(candidate["candidate"]["target"],serde_json::json!({"personId":person_id,"revisionId":revision_id}));
                    let input=serde_json::json!({"target":candidate["candidate"]["target"],"name":"Recovered Private Name","idNumber":"00000020000229007X"});
                    let operation=format!("00000000-0000-4000-a000-{:012x}",index+1);
                    let encoded=platform::cryptography::Encoded::new(&Coordination(Uuid::parse_str(&operation).expect("operation UUID"))).expect("original-operation lock encoding");let digest=platform::cryptography::sha256::Digest::new(&encoded);
                    let classid=i64::from(u32::from_be_bytes(digest.as_ref()[..4].try_into().expect("lock high half")));let objid=i64::from(u32::from_be_bytes(digest.as_ref()[4..8].try_into().expect("lock low half")));
                    proxy.arm(match index {0=>Fault::LostCommit,2=>Fault::RolledBack,3=>Fault::UnknownCommit,_=>Fault::DelayedCommit});
                    let client=http_client();let mut caller=tokio::spawn(client.post(format!("{base}/api/v1/corrections")).header("operation-id",&operation).header("session-id",&manager.0).header("cookie",&manager.1).header("x-real-ip","203.0.127.1").json(&input).timeout(Duration::from_secs(25)).send());
                    let backend=timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("actual correction write commit intercepted");let mut stop=Some(stop);
                    let unrelated=connections(&server,&name,"clerk").recover().await.expect("unrelated transaction");
                    unrelated.client().batch_execute("BEGIN; SELECT pg_advisory_xact_lock(826374)").await.expect("keep an unrelated exclusive lock alive");
                    if matches!(index,1|4) {
                        timeout(Duration::from_secs(3),async {loop {
                            let waiting:bool=observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity waiter JOIN pg_catalog.pg_locks waiting ON waiting.pid=waiter.pid JOIN pg_catalog.pg_stat_activity original ON original.pid=ANY(pg_catalog.pg_blocking_pids(waiter.pid)) WHERE waiter.datname=current_database() AND original.datname=current_database() AND waiting.locktype='advisory' AND NOT waiting.granted AND waiting.classid=$1::bigint::oid AND waiting.objid=$2::bigint::oid AND waiting.objsubid=1 AND waiter.query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock(%' AND original.state='idle in transaction')",&[&classid,&objid]).await.expect("recovery waits behind the exact original operation transaction").get(0);if waiting {break;}tokio::task::yield_now().await;
                        }}).await.expect("recovery crosses original lock");
                        assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.corrections WHERE operation_id=$1::text::uuid) AND (SELECT revision_id=$3 AND sequence=1 FROM identity.persons WHERE id=$2) AND (SELECT count(*) FROM identity.revisions WHERE person_id=$2)=1",&[&operation,&person_id,&revision_id]).await.expect("original correction remains uncommitted").get::<_,bool>(0));assert!(!caller.is_finished());
                        if index==4 {
                            caller.abort();assert!((&mut caller).await.expect_err("HTTP waiter cancelled").is_cancelled());stop.take().expect("stop owner").send(()).expect("stop with accepted correction in flight");assert!(timeout(Duration::from_millis(30),&mut task).await.is_err(),"caller cancellation does not release accepted work");
                        }
                        proxy.arm(if index==1 {Fault::ReadCommitTermination}else{Fault::LostReadCommit});proxy.release();timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("complete original readback reaches its own end fault");
                    }
                    let response=if index==4 {None}else{
                        let response=timeout(Duration::from_secs(15),&mut caller).await.expect("response budget").expect("caller").expect("HTTP result");assert_eq!(response.status(),if matches!(index,2|3) {503}else{201});assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));let response=response.json::<serde_json::Value>().await.expect("result JSON");
                        if index==2 {assert_eq!(response,serde_json::json!({"code":"dependency_unavailable"}));}
                        if index==3 {assert_eq!(response,serde_json::json!({"code":"outcome_unknown"}));assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.corrections WHERE operation_id=$1::text::uuid",&[&operation]).await.expect("unknown before actual late commit").get::<_,i64>(0),0);}
                        Some(response)
                    };
                    if matches!(index,0|3|4) {proxy.release();}
                    timeout(Duration::from_secs(5),async {
                        while observer.client().query_one(
                            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1 AND datname=current_database())", &[&backend],
                        ).await.expect("the exact original backend before observing its durable outcome").get::<_,bool>(0) {
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("original backend ended before independent facts or replay");
                    assert_eq!(unrelated.client().query_one("SELECT 1::int", &[]).await.expect("unrelated transaction does not decide the original outcome").get::<_,i32>(0),1);
                    assert!(!observer.client().query_one("SELECT pg_try_advisory_xact_lock(826374)", &[]).await.expect("the unrelated exclusive lock is still held").get::<_,bool>(0));
                    unrelated.client().batch_execute("ROLLBACK").await.expect("end unrelated transaction");
                    unrelated.terminate().await.expect("unrelated connection ended");
                    let committed=index!=2;
                    assert_eq!(observer.client().query_one(
                        "SELECT count(*) FROM identity.completions completion JOIN identity.corrections correction ON correction.operation_id=completion.operation_id JOIN identity.revisions operator ON operator.id=correction.operator_id WHERE completion.family='correction' AND completion.operation_id=$1::text::uuid AND completion.subject='session' AND completion.action='correct' AND completion.target=correction.person_id::text AND completion.person_id=operator.person_id AND completion.revision_id=operator.id AND completion.created_at=correction.corrected_at AND completion.expires_at IS NULL AND completion.transaction_id=correction.transaction_id",
                        &[&operation],
                    ).await.expect("independent permanent completion paired with the exact original correction").get::<_,i64>(0),i64::from(committed));
                    let result=timeout(Duration::from_secs(3),async {loop {
                        if let Some(row)=observer.client().query_opt("SELECT correction.id,correction.corrected_at,correction.person_id=$2 AND correction.operator_id=$3 AND correction.sequence=2 AND person.sequence=2 AND person.revision_id=correction.id AND revision.person_id=$2 AND revision.transaction_id=correction.transaction_id AS bound,(SELECT count(*) FROM identity.revisions WHERE person_id=$2)=2 AS complete FROM identity.corrections correction JOIN identity.revisions revision ON revision.id=correction.id JOIN identity.persons person ON person.id=correction.person_id WHERE correction.operation_id=$1::text::uuid",&[&operation,&person_id,&operator_revision]).await.expect("independent complete correction before replay or drain") {
                            assert!(committed);assert!(row.get::<_,bool>("bound"));assert!(row.get::<_,bool>("complete"));break Some(serde_json::json!({"code":"ok","correction":{"id":row.get::<_,Uuid>("id"),"correctedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("corrected_at")}}));
                        }
                        if !committed {assert!(observer.client().query_one("SELECT (SELECT revision_id=$2 AND sequence=1 FROM identity.persons WHERE id=$1) AND (SELECT count(*) FROM identity.revisions WHERE person_id=$1)=1",&[&person_id,&revision_id]).await.expect("confirmed rollback retains complete original identity").get::<_,bool>(0));break None;}
                        tokio::task::yield_now().await;
                    }}).await.expect("durable original outcome before replay");
                    assert_eq!(observer.client().query_one(EFFECTS,&[&account_id,&target.0,&revision_id]).await.expect("name correction leaves credentials and all old facts intact").get::<_,String>(0),effects);
                    if matches!(index,0|1) {assert_eq!(response.as_ref(),result.as_ref());}
                    if let Some(stop)=stop {stop.send(()).expect("stop original server");}let(application,ended)=timeout(Duration::from_secs(15),task).await.expect("accepted correction drains").expect("server task");ended.expect("normal drain");drop(application);
                    timeout(Duration::from_secs(3),async {loop {
                        if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename='clerk' AND pid<>pg_backend_pid())",&[]).await.expect("actual old backend termination").get::<_,bool>(0) {break;}tokio::task::yield_now().await;
                    }}).await.expect("all original backends ended before restart");
                    let application=Application::start(connections(&server,&name,"clerk"),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("startup validates real correction graph");let(base,stop,task)=running(application).await;
                    let response=http_client().post(format!("{base}/api/v1/corrections")).header("operation-id",&operation).header("session-id",&manager.0).header("cookie",&manager.1).json(&input).send().await.expect("same original operation after known restart");assert_eq!(response.status(),if committed {200}else{201});assert!(!response.headers().contains_key("set-cookie"));let replay=response.json::<serde_json::Value>().await.expect("replay");if let Some(result)=result {assert_eq!(replay,result,"late and recovered results remain fixed");}
                    let mut different=input.clone();different["name"]=serde_json::json!("Different Private Name");let response=http_client().post(format!("{base}/api/v1/corrections")).header("operation-id",&operation).header("session-id",&manager.0).header("cookie",&manager.1).json(&different).send().await.expect("altered original input");assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch"),serde_json::json!({"code":"request_mismatch"}));
                    assert!(observer.client().query_one("SELECT (SELECT count(*) FROM identity.corrections WHERE operation_id=$1::text::uuid)=1 AND (SELECT sequence AS person_sequence FROM identity.persons WHERE id=$2)=2 AND (SELECT count(*) FROM identity.revisions WHERE person_id=$2)=2",&[&operation,&person_id]).await.expect("one complete original after all replays").get::<_,bool>(0));
                    assert_eq!(observer.client().query_one(EFFECTS,&[&account_id,&target.0,&revision_id]).await.expect("no unrelated replay effects").get::<_,String>(0),effects);
                    stop.send(()).expect("stop replay server");let(application,ended)=task.await.expect("replay server");ended.expect("replay drain");drop(application);
                    assert_eq!(supplier.calls.load(Ordering::SeqCst),2);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"supplier monitoring includes cancellation, recovery, replay and complete drain");observer.terminate().await.expect("observer ended");sms.stop().await;proxy.stop().await;
                    }).await.expect("bounded original correction recovery case");
                }
                std::fs::write(std::env::var_os("NUCLEUS_TEST_CORRECTION_EXPECTATIONS").expect("private expectations path"),serde_json::to_vec(&tokens).expect("tokens JSON")).expect("save private expectations");
            }).await.expect("correction recovery scenarios budget");
        });
    });
}

#[test]
fn correction_candidate_reports_incomplete_identity_once_without_disclosing_values() {
    const OPERATION: &str = "00000000-0000-4000-a000-000000000100";
    if std::env::var_os("NUCLEUS_TEST_CORRECTION_CANDIDATE_FAILURE").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let expectations=tempfile::NamedTempFile::new().expect("private credentials");
            let child=tokio::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact","correction::correction_candidate_reports_incomplete_identity_once_without_disclosing_values","--nocapture"])
                .env("NUCLEUS_TEST_CORRECTION_CANDIDATE_FAILURE","1").env("NUCLEUS_TEST_CORRECTION_CANDIDATE_EXPECTATIONS",expectations.path())
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().expect("isolated candidate diagnostics");
            let output=timeout(Duration::from_secs(35),child.wait_with_output()).await.expect("child budget").expect("child result");let text=String::from_utf8(output.stderr).expect("JSON diagnostics");assert!(output.status.success(),"{text}");
            let events:Vec<serde_json::Value>=text.lines().map(|line|serde_json::from_str(line).expect("JSON event")).collect();let failures:Vec<_>=events.iter().filter(|event|event["level"]=="ERROR").collect();assert_eq!(failures.len(),1,"candidate read owns the only final failure: {text}");
            let failure=failures[0];assert_eq!(failure["fields"]["stage"],"correction_candidate");assert_eq!(failure["fields"]["kind"],"unavailable");assert!(failure["fields"]["error"].as_str().expect("safe crypto cause").contains("Revision("));
            let span=failure["spans"].as_array().expect("request context").iter().find(|span|span["route"]=="/api/v1/corrections/{person_id}").expect("actual candidate route");assert_eq!(span["method"],"GET");assert_eq!(span["operation_id"],OPERATION);let request_id=span["request_id"].as_str().expect("request ID");Uuid::parse_str(request_id).expect("UUID");
            let response=events.iter().find(|event|event["fields"]["stage"]=="http_response"&&event["spans"].as_array().is_some_and(|spans|spans.iter().any(|span|span["request_id"]==request_id))).expect("same request response");assert_eq!(response["fields"]["status"],503);
            let tokens:Vec<String>=serde_json::from_slice(&std::fs::read(expectations.path()).expect("private credentials")).expect("token list");
            for secret in tokens.iter().map(String::as_str).chain(["13800000000","13900000000","Fixture Registered","Fixture Operator","00000020000229007X","000000200002290010","__Host-nucleus-session","203.0.128.1",fixture::PASSWORD,"SELECT ","UPDATE "]) {assert!(!events.iter().any(|event|contains_text(event,secret)),"private material in candidate diagnostics");}
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30),async {
                let name=database(&server).await;let supplier=Supplier::echoing().await;let mut sms=fixture::supplier::Supplier::start().await;
                let application=Application::start(connections(&server,&name,"clerk"),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("startup");let(base,stop,task)=running(application).await;
                let manager=issued_session_for(&base,&mut sms,"13900000000").await;let target=issued_session_for(&base,&mut sms,"13800000000").await;
                let tokens:Vec<_>=[&manager,&target].into_iter().map(|identity|identity.1.split_once('=').expect("cookie pair").1.to_owned()).collect();std::fs::write(std::env::var_os("NUCLEUS_TEST_CORRECTION_CANDIDATE_EXPECTATIONS").expect("expected tokens path"),serde_json::to_vec(&tokens).expect("JSON tokens")).expect("private expected tokens");
                let candidate=correction_candidate(&base,&manager,"13800000000").await;assert_eq!(candidate["candidate"]["name"],"Fixture Registered");assert_eq!(candidate["candidate"]["idNumber"],"00000020000229007X");
                let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");let row=observer.client().query_one("SELECT account.person_id,person.revision_id,revision.name,revision.id_number FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id JOIN identity.revisions revision ON revision.id=person.revision_id WHERE session.id=$1::text::uuid",&[&target.0]).await.expect("actual target before controlled corruption");let person_id:Uuid=row.get("person_id");let revision_id:Uuid=row.get("revision_id");
                let owner=connections(&server,&name,"postgres").recover().await.expect("isolated corruption owner");owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("post-start integrity fixture");assert_eq!(owner.client().execute("UPDATE identity.revisions SET id_number=set_byte(id_number,octet_length(id_number)-1,get_byte(id_number,octet_length(id_number)-1)#1) WHERE id=$1",&[&revision_id]).await.expect("one authentication tag byte corrupted"),1);owner.client().batch_execute("COMMIT").await.expect("corruption committed");owner.terminate().await.expect("fixture owner ended");
                let corrupt=observer.client().query_one("SELECT name,id_number FROM identity.revisions WHERE id=$1",&[&revision_id]).await.expect("independent fault evidence before request");assert_eq!(corrupt.get::<_,Vec<u8>>("name"),row.get::<_,Vec<u8>>("name"));assert_ne!(corrupt.get::<_,Vec<u8>>("id_number"),row.get::<_,Vec<u8>>("id_number"));
                let response=http_client().get(format!("{base}/api/v1/corrections/{person_id}")).header("operation-id",OPERATION).header("session-id",&manager.0).header("cookie",&manager.1).header("x-real-ip","203.0.128.1").send().await.expect("actual candidate cannot produce half an identity");assert_eq!(response.status(),503);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("safe dependency envelope"),serde_json::json!({"code":"dependency_unavailable"}));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.corrections",&[]).await.expect("candidate made no correction").get::<_,i64>(0),0);
                stop.send(()).expect("stop");let(application,ended)=task.await.expect("server");ended.expect("drained");drop(application);
                timeout(Duration::from_secs(3),async {loop {if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename='clerk' AND pid<>pg_backend_pid())",&[]).await.expect("read resources actually ended").get::<_,bool>(0) {break;}tokio::task::yield_now().await;}}).await.expect("candidate connections ended");
                assert_eq!(supplier.calls.load(Ordering::SeqCst),2);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"candidate failure and drain never call suppliers");observer.terminate().await.expect("observer ended");sms.stop().await;
            }).await.expect("candidate integrity failure budget");
        });
    });
}

#[test]
fn correction_work_capacity_returns_retry_after_without_accepting_another_operation() {
    use fixture::database_proxy::{Fault, Proxy};

    let server = server();
    Runtime::new().expect("runtime").block_on(async {
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
            let manager = issued_session_for(&base, &mut sms, "13900000000").await;
            issued_session_for(&base, &mut sms, "13800000000").await;
            let candidate = correction_candidate(&base, &manager, "13800000000").await;
            let input = serde_json::json!({"target":candidate["candidate"]["target"],"name":"Capacity Correction","idNumber":"00000020000229007X"});
            let original = Uuid::new_v4();
            let refused = Uuid::new_v4();
            let request = |operation: Uuid, source| http_client()
                .post(format!("{base}/api/v1/corrections"))
                .header("operation-id", operation.to_string())
                .header("session-id", &manager.0).header("cookie", &manager.1)
                .header("x-real-ip", source).json(&input);
            proxy.arm(Fault::DelayedCommit);
            let accepted = tokio::spawn(request(original, "203.0.127.21").send());
            let backend = timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("accepted correction COMMIT");
            let response = request(refused, "203.0.127.22").send().await.expect("fresh source is limited by work capacity");
            assert_eq!(response.status(), 429);
            assert_eq!(response.headers()["retry-after"], "1");
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("capacity result"), serde_json::json!({"code":"rate_limited"}));
            assert!(!accepted.is_finished());
            proxy.release();
            let response = timeout(Duration::from_secs(15), accepted).await.expect("original work completes").expect("caller").expect("HTTP result");
            assert_eq!(response.status(), 201);
            let completed = response.json::<serde_json::Value>().await.expect("original completion");
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
            timeout(Duration::from_secs(5), async {
                while observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&backend]).await.expect("original backend").get::<_,bool>(0) {
                    tokio::task::yield_now().await;
                }
            }).await.expect("original backend ended before durable observation");
            assert!(observer.client().query_one(
                "SELECT (SELECT count(*) FROM identity.corrections WHERE operation_id=$1)=1 AND (SELECT count(*) FROM identity.completions WHERE family='correction' AND operation_id=$1)=1 AND NOT EXISTS(SELECT 1 FROM identity.corrections WHERE operation_id=$2) AND NOT EXISTS(SELECT 1 FROM identity.completions WHERE operation_id=$2)", &[&original,&refused],
            ).await.expect("only accepted work has permanent effects").get::<_,bool>(0));
            let replay = request(original, "203.0.127.23").send().await.expect("capacity returned after completion");
            assert_eq!(replay.status(), 200);
            assert_eq!(replay.json::<serde_json::Value>().await.expect("original replay"), completed);
            stop.send(()).expect("stop");
            let (application, ended) = task.await.expect("server");
            ended.expect("drained"); drop(application);
            observer.terminate().await.expect("observer ended");
            assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err());
            sms.stop().await; proxy.stop().await;
        }).await.expect("capacity scenario deadline");
    });
}

pub(crate) async fn correction_candidate(
    base: &str,
    manager: &(String, String),
    mobile: &str,
) -> serde_json::Value {
    let response = http_client()
        .post(format!("{base}/api/v1/corrections/candidates"))
        .header("operation-id", Uuid::new_v4().to_string())
        .header("session-id", &manager.0)
        .header("cookie", &manager.1)
        .header("x-real-ip", "203.0.124.1")
        .json(&serde_json::json!({"mobile":mobile}))
        .send()
        .await
        .expect("exact full-identity candidate");
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("set-cookie"));
    response.json().await.expect("candidate JSON")
}

pub(crate) async fn correction_protocol(
    base: &str,
    administrator: &(String, String),
    person_id: Uuid,
) {
    let client = http_client();
    let bare = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("no implicit origin");
    let path = format!("/api/v1/corrections/{person_id}");
    let request = |method, path: &str| {
        bare.request(method, format!("{base}{path}"))
            .header("operation-id", Uuid::new_v4().to_string())
            .header("session-id", &administrator.0)
            .header("cookie", &administrator.1)
            .header("x-real-ip", "203.0.126.1")
    };
    for (builder, status, code) in [
        (
            request(reqwest::Method::GET, &path),
            403,
            "permission_denied",
        ),
        (
            request(reqwest::Method::GET, &path).header("sec-fetch-site", "same-origin"),
            200,
            "ok",
        ),
        (
            request(reqwest::Method::GET, &path)
                .header("origin", "https://wrong.invalid")
                .header("sec-fetch-site", "same-origin"),
            403,
            "permission_denied",
        ),
        (
            request(reqwest::Method::GET, &path)
                .header("origin", "https://example.com")
                .header("session-id", &administrator.0),
            400,
            "invalid_input",
        ),
    ] {
        let response = builder
            .send()
            .await
            .expect("conditional read origin and unique session");
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("set-cookie"));
        assert_eq!(
            response
                .json::<serde_json::Value>()
                .await
                .expect("envelope")["code"],
            code
        );
    }
    let response = request(reqwest::Method::HEAD, &path)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await
        .expect("real automatic HEAD");
    assert_eq!(response.status(), 200);
    assert!(!response.headers().contains_key("set-cookie"));
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response.bytes().await.expect("HEAD bytes").is_empty());
    for (method, endpoint, allowed) in [
        (reqwest::Method::GET, "/api/v1/corrections", "POST"),
        (
            reqwest::Method::GET,
            "/api/v1/corrections/candidates",
            "POST",
        ),
        (reqwest::Method::POST, path.as_str(), "GET,HEAD"),
    ] {
        let response = bare
            .request(method, format!("{base}{endpoint}"))
            .header("operation-id", Uuid::new_v4().to_string())
            .header("session-id", "not-a-uuid")
            .header("x-real-ip", "not-an-address")
            .body("{")
            .send()
            .await
            .expect("unsupported method before target/authentication/body");
        assert_eq!(response.status(), 405);
        let mut methods: Vec<_> = response.headers()["allow"]
            .to_str()
            .expect("allowed methods")
            .split(',')
            .map(str::trim)
            .collect();
        methods.sort_unstable();
        assert_eq!(methods, allowed.split(',').collect::<Vec<_>>());
        assert!(!response.headers().contains_key("set-cookie"));
        assert_eq!(
            response
                .json::<serde_json::Value>()
                .await
                .expect("method envelope"),
            serde_json::json!({"code":"method_not_allowed"})
        );
    }
    let document = client
        .get(format!("{base}/openapi/v1.json"))
        .send()
        .await
        .expect("OpenAPI")
        .json::<serde_json::Value>()
        .await
        .expect("document");
    let resolve = |schema: &serde_json::Value| {
        let mut component = document
            .pointer(
                schema["$ref"]
                    .as_str()
                    .expect("schema reference")
                    .strip_prefix('#')
                    .expect("local reference"),
            )
            .expect("component");
        while let Some(reference) = component["$ref"].as_str() {
            component = document
                .pointer(reference.strip_prefix('#').expect("local reference"))
                .expect("referenced value schema");
        }
        component
    };
    for (path, method, operation) in [
        (
            "/api/v1/corrections/candidates",
            "post",
            "correctionCandidateByMobile",
        ),
        (
            "/api/v1/corrections/{person_id}",
            "get",
            "correctionCandidateByPerson",
        ),
        ("/api/v1/corrections", "post", "correctIdentity"),
    ] {
        let route = &document["paths"][path][method];
        assert_eq!(route["operationId"], operation);
        let parameters = route["parameters"].as_array().expect("parameters");
        for header in ["Operation-Id", "Session-Id"] {
            assert!(
                parameters
                    .iter()
                    .any(|parameter| parameter["name"] == header
                        && parameter["in"] == "header"
                        && parameter["required"] == true)
            );
        }
        if method == "post" {
            assert!(parameters.iter().any(
                |parameter| parameter["name"] == "Origin" && parameter["required"] == true
            ));
        } else {
            assert!(
                !parameters
                    .iter()
                    .any(|parameter| parameter["name"] == "Origin")
            );
            assert!(
                route["description"]
                    .as_str()
                    .expect("origin contract")
                    .contains("absent Origin requires Sec-Fetch-Site: same-origin")
            );
            let target = parameters
                .iter()
                .find(|parameter| parameter["name"] == "person_id")
                .expect("person path");
            assert_eq!(target["required"], true);
            let schema = resolve(&target["schema"]);
            assert_eq!(schema["type"], "string");
            assert_eq!(schema["format"], "uuid");
            assert!(document["paths"][path]["head"].is_object());
            assert!(
                document["paths"][path]["head"]["responses"]["200"]
                    .get("content")
                    .is_none()
            );
        }
        for response in route["responses"]
            .as_object()
            .expect("described responses")
            .values()
        {
            assert!(response["headers"].get("Cache-Control").is_some());
            assert!(response["headers"].get("Set-Cookie").is_none());
        }
        assert!(
            route["responses"]["429"]["headers"]
                .get("Retry-After")
                .is_some()
        );
    }
    let search = resolve(
        &document["paths"]["/api/v1/corrections/candidates"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"],
    );
    assert_eq!(search["required"], serde_json::json!(["mobile"]));
    assert_eq!(search["additionalProperties"], false);
    let input = resolve(
        &document["paths"]["/api/v1/corrections"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"],
    );
    assert_eq!(
        input["required"],
        serde_json::json!(["target", "name", "idNumber"])
    );
    assert_eq!(input["additionalProperties"], false);
    assert_eq!(
        input["properties"]
            .as_object()
            .expect("correction fields")
            .len(),
        3
    );
    let target = resolve(&input["properties"]["target"]);
    assert_eq!(
        target["required"],
        serde_json::json!(["personId", "revisionId"])
    );
    assert_eq!(target["additionalProperties"], false);
    assert_eq!(
        target["properties"]
            .as_object()
            .expect("fixed target")
            .len(),
        2
    );
    for field in ["personId", "revisionId"] {
        assert_eq!(resolve(&target["properties"][field])["format"], "uuid");
    }
    let envelope = resolve(
        &document["paths"]["/api/v1/corrections/candidates"]["post"]["responses"]["200"]["content"]
            ["application/json"]["schema"],
    );
    assert_eq!(
        envelope["required"],
        serde_json::json!(["code", "candidate"])
    );
    let candidate = resolve(&envelope["properties"]["candidate"]);
    assert_eq!(
        candidate["required"],
        serde_json::json!(["target", "name", "idNumber"])
    );
    assert_eq!(
        candidate["properties"]
            .as_object()
            .expect("complete restricted candidate")
            .len(),
        3
    );
    for field in ["name", "idNumber"] {
        assert_eq!(
            candidate["properties"][field], input["properties"][field],
            "the value owner supplies both input and complete output schema"
        );
        assert_eq!(resolve(&candidate["properties"][field])["type"], "string");
    }
    let original = &document["paths"]["/api/v1/corrections"]["post"]["responses"]["200"]["content"]
        ["application/json"]["schema"];
    assert_eq!(
        original,
        &document["paths"]["/api/v1/corrections"]["post"]["responses"]["201"]["content"]["application/json"]
            ["schema"]
    );
    let receipt = resolve(original);
    assert_eq!(
        receipt["required"],
        serde_json::json!(["code", "correction"])
    );
    let correction = resolve(&receipt["properties"]["correction"]);
    assert_eq!(
        correction["required"],
        serde_json::json!(["id", "correctedAt"])
    );
    assert_eq!(
        correction["properties"]
            .as_object()
            .expect("minimal permanent result")
            .len(),
        2
    );
    assert_eq!(resolve(&correction["properties"]["id"])["format"], "uuid");
    // Fresh sources isolate these process budgets from C1 and all earlier real writes.
    tokio::time::sleep(Duration::from_millis(120)).await;
    let started = tokio::time::Instant::now();
    for (index, endpoint) in [
        "/api/v1/corrections/candidates",
        "/api/v1/corrections",
        "/api/v1/assignments/candidates",
        "/api/v1/corrections",
        "/api/v1/corrections/candidates",
    ]
    .into_iter()
    .enumerate()
    {
        let response = client
            .post(format!("{base}{endpoint}"))
            .header("operation-id", Uuid::new_v4().to_string())
            .header("x-real-ip", "203.0.126.98")
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("shared write admission before authentication");
        assert_eq!(response.status(), if index < 4 { 401 } else { 429 });
        if index == 4 {
            assert_eq!(response.headers()["retry-after"], "1");
        }
    }
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "source has not refilled"
    );
    assert_eq!(
        client
            .get(format!("{base}{path}"))
            .header("operation-id", Uuid::new_v4().to_string())
            .header("session-id", &administrator.0)
            .header("cookie", &administrator.1)
            .header("x-real-ip", "203.0.126.98")
            .send()
            .await
            .expect("independent read budget")
            .status(),
        200
    );
    timeout(Duration::from_secs(1), async {
        let mut accepted = 0;
        loop {
            let endpoint = if accepted % 2 == 0 {
                path.as_str()
            } else {
                "/api/v1/accounts/current"
            };
            let response = client
                .get(format!("{base}{endpoint}"))
                .header("operation-id", Uuid::new_v4().to_string())
                .header("x-real-ip", "203.0.126.99")
                .send()
                .await
                .expect("shared metadata budget");
            if response.status() == 429 {
                assert!(accepted >= 40);
                assert_eq!(response.headers()["retry-after"], "1");
                break;
            }
            assert_eq!(response.status(), 401);
            accepted += 1;
            assert!(accepted < 80, "source budget must exhaust in this window");
        }
        assert_eq!(
            client
                .post(format!("{base}/api/v1/corrections/candidates"))
                .header("operation-id", Uuid::new_v4().to_string())
                .header("x-real-ip", "203.0.126.99")
                .json(&serde_json::json!({}))
                .send()
                .await
                .expect("metadata leaves write budget intact")
                .status(),
            401
        );
    })
    .await
    .expect("metadata observation window");
}
