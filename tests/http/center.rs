use std::{cell::Cell, sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use serde_json::json;
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*};

#[test]
fn affiliation_values_http_exposes_only_the_fixed_vocabulary_to_current_sessions() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(40), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(
                connections(&server, &name, "clerk"), keys(0), services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),
                supplier.client(), message_at(sms.address(), sms.authority()),
            ).await.expect("verified startup");
            let (base, stop, task) = running(application).await;
            let administrator = issued_session_for(&base, &mut sms, "13900000000").await;
            let member = issued_session_for(&base, &mut sms, "13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = Cell::new(1_u8);
            let path = "/api/v1/affiliations";
            let request = |identity: Option<&(String, String)>, method: reqwest::Method| {
                let address = source.get(); source.set(address + 1);
                let mut request = client.request(method, format!("{base}{path}"))
                    .header("operation-id", Uuid::new_v4().to_string())
                    .header("x-real-ip", format!("203.0.151.{address}"));
                if let Some(identity) = identity {
                    request = request.header("session-id", &identity.0).header("cookie", &identity.1);
                }
                request
            };
            // These expectations come from the fixed business table, not the domain enum.
            let expected = json!({"code":"ok","affiliations":[
                {"value":"01","name":"中共党员"}, {"value":"02","name":"中共预备党员"},
                {"value":"03","name":"共青团员"}, {"value":"04","name":"民革会员"},
                {"value":"05","name":"民盟盟员"}, {"value":"06","name":"民建会员"},
                {"value":"07","name":"民进会员"}, {"value":"08","name":"农工党党员"},
                {"value":"09","name":"致公党党员"}, {"value":"10","name":"九三学社社员"},
                {"value":"11","name":"台盟盟员"}, {"value":"12","name":"无党派民主人士"},
                {"value":"13","name":"群众"}
            ]});
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
            let account = observer.client().query_one(
                "SELECT account.id,account.person_id,person.revision_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1::text::uuid",
                &[&member.0],
            ).await.expect("actual member identity");
            let person_id: Uuid = account.get("person_id");
            assert!(observer.client().query_one(
                "SELECT NOT EXISTS(SELECT 1 FROM access.grants WHERE person_id=$1) AND NOT EXISTS(SELECT 1 FROM access.holders holder JOIN access.permissions permission ON permission.role_id=holder.role_id WHERE holder.person_id=$1)",
                &[&person_id],
            ).await.expect("no Workforce or management authority").get::<_, bool>(0));
            let facts = "SELECT jsonb_build_object('session',(SELECT to_jsonb(session) FROM identity.sessions session WHERE id=$1::text::uuid),'profiles',(SELECT jsonb_agg(to_jsonb(profile)) FROM center.profiles profile),'affiliations',(SELECT jsonb_agg(to_jsonb(affiliation)) FROM center.affiliations affiliation),'completions',(SELECT jsonb_agg(to_jsonb(completion)) FROM center.completions completion))::text";
            let before: String = observer.client().query_one(facts, &[&member.0]).await.expect("original persistent facts").get(0);
            for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
                for origin in [true, false] {
                    let request = request(Some(&member), method.clone());
                    let request = if origin { request.header("origin", "https://example.com") } else { request.header("sec-fetch-site", "same-origin") };
                    let response = request.send().await.expect("ordinary valid session");
                    assert_eq!(response.status(), 200);
                    assert_eq!(response.headers()["cache-control"], "no-store");
                    assert!(!response.headers().contains_key("set-cookie"));
                    assert!(!response.headers().contains_key("operation-id"));
                    if method == reqwest::Method::HEAD {
                        assert!(response.bytes().await.expect("HEAD body").is_empty());
                    } else {
                        assert_eq!(response.json::<serde_json::Value>().await.expect("fixed vocabulary"), expected);
                    }
                }
            }
            for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
                for (builder, status, code) in [
                    (request(None, method.clone()).header("origin", "https://example.com"), 401, "authentication_invalid"),
                    (request(Some(&member), method.clone()), 403, "permission_denied"),
                    (request(Some(&member), method.clone()).header("origin", "https://wrong.invalid"), 403, "permission_denied"),
                    (request(Some(&member), method.clone()).header("origin", "https://example.com").header("sec-fetch-site", "cross-site"), 403, "permission_denied"),
                    (request(Some(&member), method.clone()).header("sec-fetch-site", "same-origin").query(&[("page", "1")]), 400, "invalid_input"),
                    (request(Some(&member), method.clone()).header("sec-fetch-site", "same-origin").query(&[("extra", "true")]), 400, "invalid_input"),
                    (request(Some(&member), method.clone()).header("sec-fetch-site", "same-origin").body("{}"), 400, "invalid_input"),
                    (request(Some(&member), method.clone()).header("sec-fetch-site", "same-origin").body(vec![b'x';8193]), 413, "invalid_input"),
                    (request(None, method.clone()).header("origin", "https://example.com").header("session-id", Uuid::new_v4().to_string()).header("cookie", &member.1), 401, "authentication_invalid"),
                ] {
                    let response = builder.send().await.expect("read boundary");
                    assert_eq!(response.status(), status);
                    assert_eq!(response.headers()["cache-control"], "no-store");
                    assert!(!response.headers().contains_key("set-cookie"));
                    if method == reqwest::Method::HEAD {
                        assert!(response.bytes().await.expect("rejected HEAD body").is_empty());
                    } else {
                        assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure"), json!({"code":code}));
                    }
                }
            }
            let mut missing_operation = request(Some(&member), reqwest::Method::GET).header("origin", "https://example.com").build().expect("request");
            missing_operation.headers_mut().remove("operation-id");
            let response = client.execute(missing_operation).await.expect("required operation id");
            assert_eq!(response.status(), 400);
            assert_eq!(response.json::<serde_json::Value>().await.expect("missing operation"), json!({"code":"invalid_input"}));
            assert_eq!(observer.client().query_one(facts, &[&member.0]).await.expect("unchanged persistent facts").get::<_, String>(0), before);

            // Dictionary lookup needs no political-data access, including for an already filled person.
            let response = client.put(format!("{base}/api/v1/accounts/current/affiliation"))
                .header("operation-id", Uuid::new_v4().to_string()).header("session-id", &member.0).header("cookie", &member.1)
                .header("origin", "https://example.com").header("x-real-ip", "203.0.151.240")
                .json(&json!({"affiliation":"01"})).send().await.expect("first fill consumer");
            assert_eq!(response.status(), 200);
            let owner = connections(&server, &name, "postgres").recover().await.expect("isolated privilege owner");
            owner.client().batch_execute("REVOKE SELECT ON center.profiles,center.affiliations FROM clerk").await.expect("deny political-data reads");
            let filled: String = observer.client().query_one("SELECT to_jsonb(session)::text FROM identity.sessions session WHERE id=$1::text::uuid", &[&member.0]).await.expect("session before dictionary read").get(0);
            let response = request(Some(&member), reqwest::Method::GET).header("origin", "https://example.com").send().await.expect("dictionary independent of private values");
            assert_eq!(response.status(), 200);
            assert_eq!(response.json::<serde_json::Value>().await.expect("same fixed vocabulary"), expected);
            assert_eq!(observer.client().query_one("SELECT to_jsonb(session)::text FROM identity.sessions session WHERE id=$1::text::uuid", &[&member.0]).await.expect("session preserved").get::<_,String>(0), filled);
            owner.client().batch_execute("GRANT SELECT ON center.profiles,center.affiliations TO clerk").await.expect("restore isolated privileges");
            owner.terminate().await.expect("owner ended");

            let response = client.post(format!("{base}/api/v1/accounts/{}/suspensions", account.get::<_,Uuid>("id")))
                .header("operation-id", Uuid::new_v4().to_string()).header("session-id", &administrator.0).header("cookie", &administrator.1)
                .header("origin", "https://example.com").header("x-real-ip", "203.0.151.241")
                .json(&json!({"personId":person_id,"revisionId":account.get::<_,Uuid>("revision_id"),"mobile":"13800138000"}))
                .send().await.expect("actual account suspension");
            assert_eq!(response.status(), 201);
            for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
                let response = request(Some(&member), method.clone()).header("origin", "https://example.com").send().await.expect("paused account rejected");
                assert_eq!(response.status(), 401);
                assert!(!response.headers().contains_key("set-cookie"));
                if method == reqwest::Method::HEAD { assert!(response.bytes().await.expect("HEAD").is_empty()); }
                else { assert_eq!(response.json::<serde_json::Value>().await.expect("paused session"), json!({"code":"authentication_invalid"})); }
            }
            let spec = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("schema");
            let route = &spec["paths"][path];
            for (method, operation) in [("get", "affiliationValues"), ("head", "affiliationValuesHead")] {
                assert_eq!(route[method]["operationId"], operation);
                let parameters = route[method]["parameters"].as_array().expect("headers only");
                assert_eq!(parameters.len(), 2);
                for name in ["Operation-Id", "Session-Id"] { assert!(parameters.iter().any(|parameter| parameter["name"]==name && parameter["required"]==true)); }
                for status in ["200", "400", "401", "403", "413", "429", "500", "503"] {
                    assert!(route[method]["responses"][status].is_object());
                    assert_eq!(route[method]["responses"][status]["headers"]["Cache-Control"]["description"], "no-store");
                    if method == "head" { assert!(route[method]["responses"][status].get("content").is_none()); }
                }
            }
            assert_eq!(route["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/affiliation.Values");
            let schemas = &spec["components"]["schemas"];
            assert_eq!(schemas["affiliation.Values"]["required"], json!(["code", "affiliations"]));
            assert_eq!(schemas["affiliation.Values"]["properties"].as_object().expect("envelope").len(), 2);
            assert_eq!(schemas["affiliation.Values"]["properties"]["affiliations"]["items"]["$ref"], "#/components/schemas/affiliation.NamedCode");
            assert_eq!(schemas["affiliation.NamedCode"]["required"], json!(["value", "name"]));
            assert_eq!(schemas["affiliation.NamedCode"]["properties"].as_object().expect("item").len(), 2);
            assert_eq!(schemas["affiliation.NamedCode"]["properties"]["value"]["$ref"], "#/components/schemas/affiliation.Code");
            assert_eq!(schemas["affiliation.Code"]["enum"], json!(["01","02","03","04","05","06","07","08","09","10","11","12","13"]));
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop");
            let (application, ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
            assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err());
            sms.stop().await;
        }).await.expect("bounded fixed affiliation vocabulary HTTP contract");
    });
}

#[test]
fn affiliation_correction_http_confirms_targets_and_returns_only_completion() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(40),async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await; let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server,&name,"clerk"),keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("verified startup");
            let (base,stop,task) = running(application).await;
            let administrator = issued_session_for(&base,&mut sms,"13900000000").await;
            let member = issued_session_for(&base,&mut sms,"13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = Cell::new(1_u8);
            let request = |identity: &(String,String), method: reqwest::Method, path: &str| {
                let address=source.get(); source.set(address+1);
                client.request(method,format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&identity.0).header("cookie",&identity.1).header("x-real-ip",format!("203.0.157.{address}"))
            };
            let observer = connections(&server,&name,"clerk").recover().await.expect("independent observer");
            let row = observer.client().query_one("SELECT account.id,account.person_id,person.revision_id,session.expires_at FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1::text::uuid",&[&member.0]).await.expect("confirmed current identity");
            let account_id: Uuid=row.get("id"); let person_id: Uuid=row.get("person_id"); let revision_id: Uuid=row.get("revision_id");
            let path=format!("/api/v1/persons/{person_id}/affiliation");
            let target=json!({"type":"account","accountId":account_id,"personId":person_id,"revisionId":revision_id,"mobile":"13800138000"});
            let input=json!({"target":target,"affiliation":"01"});
            for (request,status,code) in [
                (request(&administrator,reqwest::Method::PUT,&path).json(&input),403,"permission_denied"),
                (request(&administrator,reqwest::Method::PUT,&path).header("sec-fetch-site","same-origin").json(&input),403,"permission_denied"),
                (request(&administrator,reqwest::Method::PUT,&path).header("origin","https://wrong.invalid").json(&input),403,"permission_denied"),
                (request(&administrator,reqwest::Method::PUT,&path).header("origin","https://example.com").header("sec-fetch-site","cross-site").json(&input),403,"permission_denied"),
                (request(&member,reqwest::Method::PUT,&path).header("origin","https://example.com").json(&input),403,"permission_denied"),
                (request(&administrator,reqwest::Method::PUT,&path).header("origin","https://example.com").header("content-type","application/json").body(vec![b'x';8193]),413,"invalid_input"),
                (request(&administrator,reqwest::Method::PUT,&path).header("origin","https://example.com").body("{}"),415,"invalid_input"),
            ] {
                let response=request.send().await.expect("write boundary"); assert_eq!(response.status(),status);
                assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure"),json!({"code":code}));
            }
            for input in [json!({}),json!({"target":target,"affiliation":null}),json!({"target":target,"affiliation":"14"}),json!({"target":target,"affiliation":"01","extra":true}),json!({"target":{"type":"account","accountId":account_id,"personId":person_id,"revisionId":revision_id,"mobile":null},"affiliation":"01"}),json!({"target":{"type":"account","accountId":"invalid","personId":person_id,"revisionId":revision_id},"affiliation":"01"})] {
                let response=request(&administrator,reqwest::Method::PUT,&path).header("origin","https://example.com").json(&input).send().await.expect("typed input");
                assert_eq!(response.status(),422); assert_eq!(response.json::<serde_json::Value>().await.expect("input failure"),json!({"code":"invalid_input"}));
            }
            let original=Uuid::new_v4().to_string();
            let correction=|operation:&str,input:&serde_json::Value| {
                let mut request=request(&administrator,reqwest::Method::PUT,&path).header("origin","https://example.com").json(input).build().expect("correction request");
                request.headers_mut().insert("operation-id",operation.parse().expect("operation header"));
                request
            };
            for operation in [&original,&Uuid::new_v4().to_string(),&original] {
                let response=client.execute(correction(operation,&input)).await.expect("new, same-value or replayed correction");
                assert_eq!(response.status(),200); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("minimal completion"),json!({"code":"ok"}));
            }
            let response=client.execute(correction(&original,&json!({"target":target,"affiliation":"13"}))).await.expect("different original code");
            assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch"),json!({"code":"request_mismatch"}));
            assert_eq!(history_count(&base,&administrator,person_id).await,2,"one row per new operation");
            assert_eq!(observer.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1::text::uuid",&[&member.0]).await.expect("target session unchanged").get::<_,chrono::DateTime<chrono::Utc>>(0),row.get::<_,chrono::DateTime<chrono::Utc>>("expires_at"));
            let response=request(&member,reqwest::Method::POST,"/api/v1/accounts/current/cancellations").header("origin","https://example.com").json(&json!({"accountId":account_id,"personId":person_id,"revisionId":revision_id})).send().await.expect("actual cancellation"); assert_eq!(response.status(),200);
            let response=request(&administrator,reqwest::Method::PUT,&path).header("origin","https://example.com").json(&input).send().await.expect("stale account target");
            assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("target change"),json!({"code":"target_changed"}));
            let response=request(&administrator,reqwest::Method::PUT,&path).header("origin","https://example.com").json(&json!({"target":{"type":"person","personId":person_id,"revisionId":revision_id},"affiliation":"13"})).send().await.expect("permanent person correction");
            assert_eq!(response.status(),200); assert_eq!(response.json::<serde_json::Value>().await.expect("same minimal person result"),json!({"code":"ok"}));
            assert_eq!(history_count(&base,&administrator,person_id).await,3,"permanent appended history");
            let spec=client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("schema");
            let route=&spec["paths"]["/api/v1/persons/{id}/affiliation"]["put"];
            assert_eq!(route["operationId"],"correctPersonAffiliation");
            assert!(route["parameters"].as_array().expect("headers").iter().any(|parameter|parameter["name"]=="Origin" && parameter["required"]==true));
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/affiliation.management.Input");
            let variants=spec["components"]["schemas"]["affiliation.management.Confirmation"]["oneOf"].as_array().expect("exclusive confirmation forms"); assert_eq!(variants.len(),2);
            let account=variants.iter().find(|variant|variant["properties"].get("accountId").is_some()).expect("account confirmation");
            assert!(!account["required"].as_array().expect("required identity").contains(&json!("mobile")));
            assert_eq!(account["properties"]["mobile"]["type"],"string");
            for (variant,expected) in [("account",vec!["type","accountId","personId","revisionId"]),("person",vec!["type","personId","revisionId"])] {
                let forms=spec["components"]["schemas"]["affiliation.Target"]["oneOf"].as_array().expect("candidate forms");
                let form=forms.iter().find(|form|form["properties"]["type"]["enum"]==json!([variant])).expect("candidate variant");
                let properties=form["properties"].as_object().expect("candidate fields");
                assert_eq!(properties.len(),expected.len());
                for field in expected { assert!(properties.contains_key(field),"{variant}.{field}"); }
            }
            observer.terminate().await.expect("observer ended"); stop.send(()).expect("stop"); let (application,ended)=task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2); assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded correction HTTP contract");
    });
}

#[test]
fn affiliation_management_http_enforces_visibility_and_preserves_history() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(40),async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await; let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server,&name,"clerk"),keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("verified startup");
            let (base,stop,task) = running(application).await;
            let administrator = issued_session_for(&base,&mut sms,"13900000000").await;
            let member = issued_session_for(&base,&mut sms,"13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = Cell::new(1_u8);
            let request = |identity: &(String,String), method: reqwest::Method, path: &str| {
                let address = source.get(); source.set(address+1);
                client.request(method,format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&identity.0).header("cookie",&identity.1).header("x-real-ip",format!("203.0.156.{address}"))
            };
            let observer = connections(&server,&name,"clerk").recover().await.expect("independent observer");
            let row = observer.client().query_one("SELECT account.id,account.person_id,person.revision_id,session.expires_at FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1::text::uuid",&[&member.0]).await.expect("current identity facts");
            let account_id: Uuid = row.get("id"); let person_id: Uuid = row.get("person_id"); let revision_id: Uuid = row.get("revision_id");
            let path = format!("/api/v1/persons/{person_id}/affiliation"); let candidate_path = format!("/api/v1/affiliations/candidates/{person_id}");
            let history_path = format!("/api/v1/persons/{person_id}/affiliations");
            let candidate = json!({"code":"ok","candidate":{"target":{"type":"account","accountId":account_id,"personId":person_id,"revisionId":revision_id},"name":"F*"}});
            for request in [request(&administrator,reqwest::Method::POST,"/api/v1/affiliations/candidates").header("origin","https://example.com").json(&json!({"mobile":"13800138000"})),request(&administrator,reqwest::Method::GET,&candidate_path).header("sec-fetch-site","same-origin")] {
                let response = request.send().await.expect("qualified candidate"); assert_eq!(response.status(),200);
                assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("minimal candidate"),candidate);
            }
            for target in [&path,&candidate_path,&history_path] {
                for (request,status,code) in [
                    (request(&member,reqwest::Method::GET,target).header("origin","https://example.com"),403,"permission_denied"),
                    (request(&administrator,reqwest::Method::GET,target),403,"permission_denied"),
                    (request(&administrator,reqwest::Method::GET,target).header("origin","https://wrong.invalid"),403,"permission_denied"),
                    (request(&administrator,reqwest::Method::GET,target).header("origin","https://example.com").header("sec-fetch-site","cross-site"),403,"permission_denied"),
                    (request(&administrator,reqwest::Method::GET,target).header("sec-fetch-site","same-origin").query(&[("extra","true")]),400,"invalid_input"),
                    (request(&administrator,reqwest::Method::GET,target).header("sec-fetch-site","same-origin").body("{}"),400,"invalid_input"),
                    (request(&administrator,reqwest::Method::GET,target).header("sec-fetch-site","same-origin").body(vec![b'x';8193]),413,"invalid_input"),
                ] {
                    let response = request.send().await.expect("GET boundary"); assert_eq!(response.status(),status);
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure"),json!({"code":code}));
                }
            }
            for path in ["/api/v1/persons/invalid/affiliation","/api/v1/affiliations/candidates/invalid","/api/v1/persons/invalid/affiliations"] {
                let response = client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).send().await.expect("path parsed before identity");
                assert_eq!(response.status(),400); assert_eq!(response.json::<serde_json::Value>().await.expect("safe path failure"),json!({"code":"invalid_input"}));
            }
            for input in [json!({}),json!({"mobile":null}),json!({"mobile":"invalid"}),json!({"mobile":13800138000_i64}),json!({"mobile":"13800138000","extra":true})] {
                let response = request(&administrator,reqwest::Method::POST,"/api/v1/affiliations/candidates").header("origin","https://example.com").json(&input).send().await.expect("typed mobile");
                assert_eq!(response.status(),422); assert_eq!(response.json::<serde_json::Value>().await.expect("safe input failure"),json!({"code":"invalid_input"}));
            }
            let response = request(&administrator,reqwest::Method::POST,"/api/v1/affiliations/candidates").json(&json!({"mobile":"13800138000"})).send().await.expect("mandatory POST origin"); assert_eq!(response.status(),403);
            let response = request(&administrator,reqwest::Method::GET,&path).header("sec-fetch-site","same-origin").send().await.expect("unfilled existing person");
            assert_eq!(response.status(),404); assert_eq!(response.json::<serde_json::Value>().await.expect("unset"),json!({"code":"affiliation_unset"}));
            let response = request(&administrator,reqwest::Method::GET,&format!("/api/v1/persons/{}/affiliation",Uuid::new_v4())).header("sec-fetch-site","same-origin").send().await.expect("unknown person");
            assert_eq!(response.status(),404); assert_eq!(response.json::<serde_json::Value>().await.expect("missing"),json!({"code":"target_unavailable"}));
            for method in [reqwest::Method::GET,reqwest::Method::HEAD] {
                let response = request(&administrator,method.clone(),&history_path).header("sec-fetch-site","same-origin").send().await.expect("unfilled history");
                assert_eq!(response.status(),200);
                if method==reqwest::Method::HEAD { assert!(response.bytes().await.expect("empty HEAD").is_empty()); }
                else { assert_eq!(response.json::<serde_json::Value>().await.expect("known empty history"),json!({"code":"ok","count":0,"affiliations":[]})); }
            }
            let administrator_id = uuid_of(&current_account(&base,&administrator).await["personId"]);
            for id in [Uuid::new_v4(),administrator_id] {
                let response = request(&administrator,reqwest::Method::GET,&format!("/api/v1/persons/{id}/affiliations")).header("origin","https://example.com").send().await.expect("unknown or self target");
                assert_eq!(response.status(),404); assert_eq!(response.json::<serde_json::Value>().await.expect("unavailable history target"),json!({"code":"target_unavailable"}));
            }
            for query in ["?page=0","?page=-1","?size=0","?size=101","?page=1.5","?page=null","?page=1&page=2","?size=1&size=2","?unknown=1","?page=9223372036854775807&size=100"] {
                let response = request(&administrator,reqwest::Method::GET,&format!("{history_path}{query}")).header("origin","https://example.com").send().await.expect("invalid history pagination");
                assert_eq!(response.status(),400,"{query}"); assert_eq!(response.json::<serde_json::Value>().await.expect("safe pagination failure"),json!({"code":"invalid_input"}));
            }
            for (request,status,code) in [
                (client.get(format!("{base}{history_path}")).header("operation-id",Uuid::new_v4().to_string()).header("origin","https://example.com").header("x-real-ip","203.0.156.240"),401,"authentication_invalid"),
                (client.get(format!("{base}{history_path}")).header("session-id",&administrator.0).header("cookie",&administrator.1).header("origin","https://example.com").header("x-real-ip","203.0.156.241"),400,"invalid_input"),
            ] {
                let response = request.send().await.expect("required history headers"); assert_eq!(response.status(),status);
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe header failure"),json!({"code":code}));
            }
            let response = request(&member,reqwest::Method::PUT,"/api/v1/accounts/current/affiliation").header("origin","https://example.com").json(&json!({"affiliation":"01"})).send().await.expect("actual first fill"); assert_eq!(response.status(),200);
            let value = observer.client().query_one("SELECT value,created_at FROM center.affiliations WHERE person_id=$1",&[&person_id]).await.expect("original persisted value");
            let created_at: chrono::DateTime<chrono::Utc> = value.get("created_at");
            let history = json!({"code":"ok","count":1,"affiliations":[{"value":"01","createdAt":created_at,"operator":{"personId":person_id,"revisionId":revision_id,"name":"F*"}}]});
            for target in [&path,&candidate_path,&history_path] {
                for method in [reqwest::Method::GET,reqwest::Method::HEAD] {
                    let response = request(&administrator,method.clone(),target).header("sec-fetch-site","same-origin").send().await.expect("qualified management read");
                    assert_eq!(response.status(),200); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    if method == reqwest::Method::HEAD { assert!(response.bytes().await.expect("HEAD body").is_empty()); }
                    else { assert_eq!(response.json::<serde_json::Value>().await.expect("minimal response"),if target==&path {json!({"code":"ok","affiliation":{"value":"01","createdAt":created_at}})} else if target==&history_path {history.clone()} else {candidate.clone()}); }
                }
            }
            for (query,present) in [("?page=1&size=1",true),("?size=100",true),("?page=2&size=1",false),("?page=9223372036854775807&size=1",false)] {
                let response = request(&administrator,reqwest::Method::GET,&format!("{history_path}{query}")).header("origin","https://example.com").send().await.expect("history page");
                assert_eq!(response.status(),200);
                assert_eq!(response.json::<serde_json::Value>().await.expect("exact count and masked page"),if present {history.clone()} else {json!({"code":"ok","count":1,"affiliations":[]})});
            }
            let owner = connections(&server,&name,"postgres").recover().await.expect("isolated privilege owner");
            owner.client().batch_execute("REVOKE SELECT ON center.affiliations FROM clerk").await.expect("isolated query denial");
            let response = request(&administrator,reqwest::Method::GET,&path).header("sec-fetch-site","same-origin").send().await.expect("value read failure");
            assert_eq!(response.status(),503); assert_eq!(response.json::<serde_json::Value>().await.expect("safe dependency failure"),json!({"code":"dependency_unavailable"}));
            let response = request(&administrator,reqwest::Method::GET,&history_path).header("sec-fetch-site","same-origin").send().await.expect("history read failure");
            assert_eq!(response.status(),503); assert_eq!(response.json::<serde_json::Value>().await.expect("no false empty history"),json!({"code":"dependency_unavailable"}));
            let response = request(&administrator,reqwest::Method::GET,&candidate_path).header("sec-fetch-site","same-origin").send().await.expect("candidate independent of values");
            assert_eq!(response.status(),200); assert_eq!(response.json::<serde_json::Value>().await.expect("candidate unchanged"),candidate);
            owner.client().batch_execute("GRANT SELECT ON center.affiliations TO clerk").await.expect("restore isolated query privilege"); owner.terminate().await.expect("owner ended");
            let after = observer.client().query_one("SELECT affiliation.value,affiliation.created_at,session.expires_at FROM center.affiliations affiliation JOIN identity.sessions session ON session.id=$1::text::uuid WHERE affiliation.person_id=$2",&[&member.0,&person_id]).await.expect("read effects");
            assert_eq!(after.get::<_,Vec<u8>>("value"),value.get::<_,Vec<u8>>("value")); assert_eq!(after.get::<_,chrono::DateTime<chrono::Utc>>("created_at"),created_at); assert_eq!(after.get::<_,chrono::DateTime<chrono::Utc>>("expires_at"),row.get::<_,chrono::DateTime<chrono::Utc>>("expires_at"));
            let response = request(&member,reqwest::Method::POST,"/api/v1/accounts/current/cancellations").header("origin","https://example.com").json(&json!({"accountId":account_id,"personId":person_id,"revisionId":revision_id})).send().await.expect("actual cancellation"); assert_eq!(response.status(),200);
            let response = request(&administrator,reqwest::Method::GET,&candidate_path).header("sec-fetch-site","same-origin").send().await.expect("retained person candidate");
            assert_eq!(response.status(),200); assert_eq!(response.json::<serde_json::Value>().await.expect("no absent account field"),json!({"code":"ok","candidate":{"target":{"type":"person","personId":person_id,"revisionId":revision_id},"name":"F*"}}));
            let response = request(&administrator,reqwest::Method::GET,&path).header("sec-fetch-site","same-origin").send().await.expect("cancelled person value"); assert_eq!(response.status(),200);
            let response = request(&administrator,reqwest::Method::GET,&history_path).header("sec-fetch-site","same-origin").send().await.expect("cancelled person history");
            assert_eq!(response.status(),200); assert_eq!(response.json::<serde_json::Value>().await.expect("permanent history and fixed operator"),history);
            let spec = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("schema");
            for route in ["/api/v1/affiliations/candidates/{person_id}","/api/v1/persons/{id}/affiliation","/api/v1/persons/{id}/affiliations"] {
                for method in ["get","head"] {
                    let operation = &spec["paths"][route][method]; assert!(operation.is_object());
                    let parameters = operation["parameters"].as_array().expect("parameters");
                    assert!(!parameters.iter().any(|parameter|parameter["name"]=="Origin"));
                    for header in ["Operation-Id","Session-Id"] { assert!(parameters.iter().any(|parameter|parameter["name"]==header && parameter["required"]==true)); }
                }
                assert!(spec["paths"][route]["head"]["responses"]["200"].get("content").is_none());
            }
            let route = &spec["paths"]["/api/v1/persons/{id}/affiliations"];
            for (method,operation) in [("get","personAffiliationHistory"),("head","personAffiliationHistoryHead")] {
                assert_eq!(route[method]["operationId"],operation);
                let parameters = route[method]["parameters"].as_array().expect("history parameters");
                for name in ["page","size"] { assert!(parameters.iter().any(|parameter|parameter["name"]==name && parameter["required"]==false)); }
                assert!(parameters.iter().any(|parameter|parameter["name"]=="id" && parameter["required"]==true));
            }
            assert_eq!(route["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/affiliation.management.History");
            let history = &spec["components"]["schemas"]["affiliation.management.History"];
            assert_eq!(history["required"],json!(["code","count","affiliations"])); assert_eq!(history["properties"].as_object().expect("minimal history envelope").len(),3);
            assert_eq!(history["properties"]["affiliations"]["items"]["$ref"],"#/components/schemas/affiliation.management.Affiliation");
            let item = &spec["components"]["schemas"]["affiliation.management.Affiliation"];
            assert_eq!(item["required"],json!(["value","createdAt","operator"])); assert_eq!(item["properties"].as_object().expect("minimal managed history item").len(),3);
            assert_eq!(item["properties"]["operator"]["$ref"],"#/components/schemas/person.revision.Identity");
            let candidate_schema = &spec["components"]["schemas"]["affiliation.Candidate"];
            assert_eq!(candidate_schema["properties"].as_object().expect("minimal candidate fields").len(),2); assert_eq!(candidate_schema["required"],json!(["target","name"]));
            let variants = spec["components"]["schemas"]["affiliation.Target"]["oneOf"].as_array().expect("exclusive target forms"); assert_eq!(variants.len(),2);
            let post = &spec["paths"]["/api/v1/affiliations/candidates"]["post"];
            assert!(post["parameters"].as_array().expect("POST headers").iter().any(|parameter|parameter["name"]=="Origin" && parameter["required"]==true));
            assert_eq!(post["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/affiliation.Search");
            observer.terminate().await.expect("observer ended"); stop.send(()).expect("stop"); let (application,ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2); assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded management HTTP contract");
    });
}

#[test]
fn affiliation_http_preserves_identity_and_exposes_only_qualified_current_values() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(40),async {
            let name=database(&server).await;
            let supplier=Supplier::echoing().await; let mut sms=fixture::supplier::Supplier::start().await;
            let application=Application::start(connections(&server,&name,"clerk"),keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("verified startup");
            let(base,stop,task)=running(application).await;
            let administrator=issued_session_for(&base,&mut sms,"13900000000").await;
            let member=issued_session_for(&base,&mut sms,"13800138000").await;
            let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source=Cell::new(1_u8);let path="/api/v1/accounts/current/affiliation";
            let request=|identity:&(String,String),operation:Uuid| {
                let address=source.get();source.set(address+1);
                client.put(format!("{base}{path}")).header("operation-id",operation.to_string()).header("session-id",&identity.0).header("cookie",&identity.1).header("x-real-ip",format!("203.0.151.{address}"))
            };
            let read=|identity:&(String,String),method:reqwest::Method| {
                let address=source.get();source.set(address+1);
                client.request(method,format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&identity.0).header("cookie",&identity.1).header("x-real-ip",format!("203.0.151.{address}"))
            };
            for method in [reqwest::Method::GET,reqwest::Method::HEAD] {
                let response=read(&member,method.clone()).header("sec-fetch-site","same-origin").send().await.expect("unfilled current value");
                assert_eq!(response.status(),404);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                if method==reqwest::Method::HEAD {assert!(response.bytes().await.expect("HEAD body").is_empty());}
                else {assert_eq!(response.json::<serde_json::Value>().await.expect("unset result"),json!({"code":"affiliation_unset"}));}
            }
            for (request,status,code) in [
                (read(&member,reqwest::Method::GET),403,"permission_denied"),
                (read(&member,reqwest::Method::GET).header("origin","https://wrong.invalid"),403,"permission_denied"),
                (read(&member,reqwest::Method::GET).header("origin","https://example.com").header("sec-fetch-site","cross-site"),403,"permission_denied"),
                (client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("x-real-ip","203.0.151.203").header("origin","https://example.com"),401,"authentication_invalid"),
                (client.get(format!("{base}{path}")).header("session-id",&member.0).header("cookie",&member.1).header("x-real-ip","203.0.151.204").header("origin","https://example.com"),400,"invalid_input"),
                (read(&member,reqwest::Method::GET).header("origin","https://example.com").query(&[("extra","true")]),400,"invalid_input"),
                (read(&member,reqwest::Method::GET).header("origin","https://example.com").body("{}"),400,"invalid_input"),
                (read(&member,reqwest::Method::GET).header("origin","https://example.com").body(vec![b'x';8193]),413,"invalid_input"),
            ] {
                let response=request.send().await.expect("read HTTP boundary");assert_eq!(response.status(),status);
                assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("read failure"),json!({"code":code}));
            }
            for (request,status,code) in [
                (request(&member,Uuid::new_v4()).json(&json!({"affiliation":"01"})),403,"permission_denied"),
                (request(&member,Uuid::new_v4()).header("origin","https://wrong.invalid").json(&json!({"affiliation":"01"})),403,"permission_denied"),
                (request(&member,Uuid::new_v4()).header("origin","https://example.com").header("sec-fetch-site","cross-site").json(&json!({"affiliation":"01"})),403,"permission_denied"),
                (client.put(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("x-real-ip","203.0.151.200").header("origin","https://example.com").json(&json!({"affiliation":"01"})),401,"authentication_invalid"),
                (client.put(format!("{base}{path}")).header("session-id",&member.0).header("cookie",&member.1).header("x-real-ip","203.0.151.201").header("origin","https://example.com").json(&json!({"affiliation":"01"})),400,"invalid_input"),
            ] {
                let response=request.send().await.expect("HTTP boundary"); assert_eq!(response.status(),status);
                assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("failure"),json!({"code":code}));
            }
            for input in [json!({}),json!({"affiliation":null}),json!({"affiliation":1}),json!({"affiliation":["01"]}),json!({"affiliation":"1"}),json!({"affiliation":"14"}),json!({"affiliation":"群众"}),json!({"affiliation":"01","extra":true})] {
                let response=request(&member,Uuid::new_v4()).header("origin","https://example.com").json(&input).send().await.expect("typed input");
                assert_eq!(response.status(),422);assert_eq!(response.json::<serde_json::Value>().await.expect("invalid input"),json!({"code":"invalid_input"}));
            }
            let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
            assert_eq!(own_history_count(&base,&member).await+own_history_count(&base,&administrator).await,0,"refusals made no facts");
            let original=Uuid::new_v4();
            for (identity,operation,code,status,result) in [
                (&member,original,"01",200,"ok"),
                (&member,original,"01",200,"ok"),
                (&member,original,"13",409,"request_mismatch"),
                (&member,Uuid::new_v4(),"01",409,"state_conflict"),
                (&administrator,original,"01",409,"request_mismatch"),
            ] {
                let response=request(identity,operation).header("origin","https://example.com").json(&json!({"affiliation":code})).send().await.expect("first fill or replay");
                assert_eq!(response.status(),status);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("minimal result"),json!({"code":result}));
                assert_eq!(own_history_count(&base,&member).await+own_history_count(&base,&administrator).await,1,"one permanent result after each request");
            }
            let row=observer.client().query_one("SELECT affiliation.person_id=account.person_id AS own, affiliation.operator_id=person.revision_id AS current, octet_length(affiliation.value) AS sealed, affiliation.value, affiliation.created_at, session.expires_at FROM center.affiliations affiliation JOIN identity.sessions session ON session.id=$1::text::uuid JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE affiliation.operation_id=$2",&[&member.0,&original]).await.expect("independent HTTP effect");
            assert!(row.get::<_,bool>("own") && row.get::<_,bool>("current"));assert_eq!(row.get::<_,i32>("sealed"),39);
            let created_at=row.get::<_,chrono::DateTime<chrono::Utc>>("created_at");
            for request in [read(&member,reqwest::Method::GET).header("origin","https://example.com"),read(&member,reqwest::Method::GET).header("sec-fetch-site","same-origin")] {
                let response=request.send().await.expect("qualified current value");assert_eq!(response.status(),200);
                assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("minimal current projection"),json!({"code":"ok","affiliation":{"value":"01","createdAt":created_at}}));
            }
            let response=read(&member,reqwest::Method::HEAD).header("origin","https://example.com").send().await.expect("current HEAD");
            assert_eq!(response.status(),200);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));assert!(response.bytes().await.expect("empty HEAD").is_empty());
            let response=read(&administrator,reqwest::Method::GET).header("origin","https://example.com").send().await.expect("separate person's current value");
            assert_eq!(response.status(),404);assert_eq!(response.json::<serde_json::Value>().await.expect("own unset result"),json!({"code":"affiliation_unset"}));
            let retained=observer.client().query_one("SELECT affiliation.value, affiliation.created_at, session.expires_at, (SELECT count(*) FROM center.affiliations) AS count FROM center.affiliations affiliation JOIN identity.sessions session ON session.id=$1::text::uuid WHERE affiliation.operation_id=$2",&[&member.0,&original]).await.expect("independent read effects");
            assert_eq!(retained.get::<_,Vec<u8>>("value"),row.get::<_,Vec<u8>>("value"));assert_eq!(retained.get::<_,chrono::DateTime<chrono::Utc>>("created_at"),created_at);
            assert_eq!(retained.get::<_,chrono::DateTime<chrono::Utc>>("expires_at"),row.get::<_,chrono::DateTime<chrono::Utc>>("expires_at"));assert_eq!(retained.get::<_,i64>("count"),1);
            let spec=client.get(format!("{base}/openapi/v1.json")).header("x-real-ip","203.0.151.202").send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("schema");
            let route=&spec["paths"][path]["put"];assert_eq!(route["operationId"],"createCurrentAffiliation");
            for (method,operation) in [("get","currentAffiliation"),("head","currentAffiliationHead")] {
                let route=&spec["paths"][path][method];assert_eq!(route["operationId"],operation);
                let parameters=route["parameters"].as_array().expect("read parameters");
                for header in ["Operation-Id","Session-Id"] {assert!(parameters.iter().any(|parameter|parameter["name"]==header && parameter["required"]==true));}
                assert!(!parameters.iter().any(|parameter|parameter["name"]=="Origin"));assert!(route["responses"].get("404").is_some());
            }
            assert_eq!(spec["paths"][path]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/affiliation.Current");
            assert!(spec["paths"][path]["head"]["responses"]["200"].get("content").is_none());
            let current=&spec["components"]["schemas"]["affiliation.Current"];assert_eq!(current["properties"]["affiliation"]["$ref"],"#/components/schemas/affiliation.Affiliation");
            let affiliation=&spec["components"]["schemas"]["affiliation.Affiliation"];assert_eq!(affiliation["properties"].as_object().expect("value and effective time only").len(),2);
            assert_eq!(affiliation["required"],json!(["value","createdAt"]));assert_eq!(affiliation["properties"]["value"]["$ref"],"#/components/schemas/affiliation.Code");
            for header in ["Operation-Id","Session-Id","Origin"] {
                assert!(route["parameters"].as_array().expect("parameters").iter().any(|parameter|parameter["name"]==header && parameter["required"]==true));
            }
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/affiliation.Input");
            assert_eq!(spec["components"]["schemas"]["affiliation.Code"]["enum"],json!(["01","02","03","04","05","06","07","08","09","10","11","12","13"]));
            observer.terminate().await.expect("observer ended"); stop.send(()).expect("stop");let(application,ended)=task.await.expect("process");ended.expect("drained");drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err());sms.stop().await;
        }).await.expect("bounded HTTP first fill");
    });
}

#[test]
fn affiliation_history_http_returns_only_the_current_persons_paginated_values() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(35), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await; let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server,&name,"clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("verified startup");
            let (base,stop,task) = running(application).await;
            let administrator = issued_session_for(&base,&mut sms,"13900000000").await;
            let member = issued_session_for(&base,&mut sms,"13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = Cell::new(1_u8); let path = "/api/v1/accounts/current/affiliations";
            let read = |identity: &(String,String), method: reqwest::Method, query: &str| {
                let address = source.get(); source.set(address+1);
                client.request(method,format!("{base}{path}{query}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&identity.0).header("cookie",&identity.1).header("x-real-ip",format!("203.0.155.{address}"))
            };
            for method in [reqwest::Method::GET,reqwest::Method::HEAD] {
                let response = read(&member,method.clone(),"").header("sec-fetch-site","same-origin").send().await.expect("empty history");
                assert_eq!(response.status(),200); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                if method == reqwest::Method::HEAD { assert!(response.bytes().await.expect("empty HEAD").is_empty()); }
                else { assert_eq!(response.json::<serde_json::Value>().await.expect("empty list"),json!({"code":"ok","count":0,"affiliations":[]})); }
            }
            for query in ["?page=0","?page=-1","?size=0","?size=101","?page=1.5","?page=null","?page=1&page=2","?size=1&size=2","?unknown=1","?page=9223372036854775807&size=100"] {
                let response = read(&member,reqwest::Method::GET,query).header("origin","https://example.com").send().await.expect("invalid pagination");
                assert_eq!(response.status(),400,"{query}"); assert_eq!(response.json::<serde_json::Value>().await.expect("input failure"),json!({"code":"invalid_input"}));
            }
            for (request,status,code) in [
                (read(&member,reqwest::Method::GET,"").header("origin","https://example.com").body("x"),400,"invalid_input"),
                (read(&member,reqwest::Method::GET,"").header("origin","https://example.com").body(vec![b'x';8193]),413,"invalid_input"),
                (read(&member,reqwest::Method::GET,""),403,"permission_denied"),
                (read(&member,reqwest::Method::GET,"").header("origin","https://wrong.invalid"),403,"permission_denied"),
                (read(&member,reqwest::Method::GET,"").header("origin","https://example.com").header("sec-fetch-site","cross-site"),403,"permission_denied"),
                (client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("x-real-ip","203.0.155.200").header("origin","https://example.com"),401,"authentication_invalid"),
                (client.get(format!("{base}{path}")).header("session-id",&member.0).header("cookie",&member.1).header("x-real-ip","203.0.155.201").header("origin","https://example.com"),400,"invalid_input"),
            ] {
                let response = request.send().await.expect("rejected read"); assert_eq!(response.status(),status);
                assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe rejection"),json!({"code":code}));
            }
            let original = Uuid::new_v4();
            let response = http_client().put(format!("{base}/api/v1/accounts/current/affiliation")).header("session-id",&member.0).header("cookie",&member.1).header("operation-id",original.to_string()).header("x-real-ip","203.0.155.202").json(&json!({"affiliation":"01"})).send().await.expect("actual first fill");
            assert_eq!(response.status(),200);
            let observer = connections(&server,&name,"clerk").recover().await.expect("independent observer");
            let facts = "SELECT affiliation.value,affiliation.created_at,session.expires_at,(SELECT count(*) FROM center.affiliations) AS count FROM center.affiliations affiliation JOIN identity.sessions session ON session.id=$1::text::uuid WHERE affiliation.operation_id=$2";
            let before = observer.client().query_one(facts,&[&member.0,&original]).await.expect("original stored facts");
            let created_at: chrono::DateTime<chrono::Utc> = before.get("created_at");
            for (query,expected) in [("",true),("?page=1&size=1",true),("?size=100",true),("?page=2&size=1",false),("?page=9223372036854775807&size=1",false)] {
                let response = read(&member,reqwest::Method::GET,query).header("origin","https://example.com").send().await.expect("history page");
                assert_eq!(response.status(),200); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                let affiliations = if expected { json!([{"value":"01","createdAt":created_at}]) } else { json!([]) };
                assert_eq!(response.json::<serde_json::Value>().await.expect("exact self-visible response"),json!({"code":"ok","count":1,"affiliations":affiliations}));
            }
            let response = read(&member,reqwest::Method::HEAD,"?size=1").header("sec-fetch-site","same-origin").send().await.expect("nonempty history HEAD");
            assert_eq!(response.status(),200); assert!(response.bytes().await.expect("HEAD body").is_empty());
            let response = read(&administrator,reqwest::Method::GET,"").header("origin","https://example.com").send().await.expect("administrator's own scope");
            assert_eq!(response.status(),200); assert_eq!(response.json::<serde_json::Value>().await.expect("private person history"),json!({"code":"ok","count":0,"affiliations":[]}));
            let after = observer.client().query_one(facts,&[&member.0,&original]).await.expect("read effects");
            assert_eq!(after.get::<_,Vec<u8>>("value"),before.get::<_,Vec<u8>>("value")); assert_eq!(after.get::<_,chrono::DateTime<chrono::Utc>>("created_at"),created_at);
            assert_eq!(after.get::<_,chrono::DateTime<chrono::Utc>>("expires_at"),before.get::<_,chrono::DateTime<chrono::Utc>>("expires_at")); assert_eq!(after.get::<_,i64>("count"),1);
            let spec = client.get(format!("{base}/openapi/v1.json")).header("x-real-ip","203.0.155.203").send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("schema");
            for (method,operation) in [("get","currentAffiliationHistory"),("head","currentAffiliationHistoryHead")] {
                let route = &spec["paths"][path][method]; assert_eq!(route["operationId"],operation);
                let parameters = route["parameters"].as_array().expect("parameters");
                for header in ["Operation-Id","Session-Id"] { assert!(parameters.iter().any(|value|value["name"]==header && value["required"]==true)); }
                for name in ["page","size"] { assert!(parameters.iter().any(|value|value["name"]==name && value["required"]==false)); }
                assert!(!parameters.iter().any(|value|value["name"]=="Origin")); assert!(route["responses"].get("404").is_none());
            }
            assert_eq!(spec["paths"][path]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/affiliation.History");
            assert!(spec["paths"][path]["head"]["responses"]["200"].get("content").is_none());
            let history = &spec["components"]["schemas"]["affiliation.History"];
            assert_eq!(history["required"],json!(["code","count","affiliations"])); assert_eq!(history["properties"].as_object().expect("minimal envelope").len(),3);
            assert_eq!(history["properties"]["affiliations"]["items"]["$ref"],"#/components/schemas/affiliation.Affiliation");
            observer.terminate().await.expect("observer ended"); stop.send(()).expect("stop"); let (application,ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2); assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded HTTP self history");
    });
}

#[test]
fn affiliation_failures_retain_safe_read_and_cancelled_write_diagnostics() {
    const OPERATION: &str = "00000000-0000-4000-8000-000000000109";
    const READ_OPERATION: &str = "00000000-0000-4000-8000-000000000110";
    const HISTORY_OPERATION: &str = "00000000-0000-4000-8000-000000000111";
    const MANAGEMENT_OPERATION: &str = "00000000-0000-4000-8000-000000000112";
    if std::env::var_os("NUCLEUS_TEST_AFFILIATION_LOG").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let expected = tempfile::NamedTempFile::new().expect("private expected secrets");
            let child = tokio::process::Command::new(
                std::env::current_exe().expect("test executable"),
            )
            .args([
                "--exact",
                "center::affiliation_failures_retain_safe_read_and_cancelled_write_diagnostics",
                "--nocapture",
            ])
            .env("NUCLEUS_TEST_AFFILIATION_LOG", expected.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("isolated production subscriber");
            let output = timeout(Duration::from_secs(45), child.wait_with_output())
                .await
                .expect("child deadline")
                .expect("child output");
            let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
            assert!(output.status.success(), "{text}");
            let events: Vec<serde_json::Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).expect("JSON event"))
                .collect();
            let errors: Vec<_> = events
                .iter()
                .filter(|event| event["level"] == "ERROR")
                .collect();
            assert_eq!(
                errors.len(),
                4,
                "one failure per read and one final write failure through drain: {text}"
            );
            let read = errors
                .iter()
                .find(|event| event["fields"]["stage"] == "affiliation_read")
                .expect("read failure recorded once");
            assert_eq!(read["target"], "nucleus::route");
            assert_eq!(read["fields"]["kind"], "unavailable");
            assert!(
                read["fields"]["error"]
                    .as_str()
                    .expect("safe query cause")
                    .contains("Query")
            );
            let span = read["spans"]
                .as_array()
                .expect("read spans")
                .iter()
                .find(|span| span["route"] == "/api/v1/accounts/current/affiliation")
                .expect("read request context");
            assert_eq!(span["method"], "GET");
            assert_eq!(span["operation_id"], READ_OPERATION);
            Uuid::parse_str(span["request_id"].as_str().expect("read request ID"))
                .expect("server request UUID");
            let history = errors
                .iter()
                .find(|event| event["fields"]["stage"] == "affiliation_history")
                .expect("history failure recorded once");
            assert_eq!(history["target"], "nucleus::route");
            assert_eq!(history["fields"]["kind"], "unavailable");
            assert!(
                history["fields"]["error"]
                    .as_str()
                    .expect("safe history cause")
                    .contains("Query")
            );
            let span = history["spans"]
                .as_array()
                .expect("history spans")
                .iter()
                .find(|span| span["route"] == "/api/v1/accounts/current/affiliations")
                .expect("history request context");
            assert_eq!(span["method"], "GET");
            assert_eq!(span["operation_id"], HISTORY_OPERATION);
            Uuid::parse_str(span["request_id"].as_str().expect("history request ID"))
                .expect("server request UUID");
            let management = errors
                .iter()
                .find(|event| event["fields"]["stage"] == "affiliation_management_history")
                .expect("management history failure recorded once");
            assert_eq!(management["target"], "nucleus::route");
            assert_eq!(management["fields"]["kind"], "unavailable");
            assert!(
                management["fields"]["error"]
                    .as_str()
                    .expect("safe management query cause")
                    .contains("Query")
            );
            let span = management["spans"]
                .as_array()
                .expect("management spans")
                .iter()
                .find(|span| span["route"] == "/api/v1/persons/{id}/affiliations")
                .expect("management route context");
            assert_eq!(span["method"], "GET");
            assert_eq!(span["operation_id"], MANAGEMENT_OPERATION);
            Uuid::parse_str(span["request_id"].as_str().expect("management request ID"))
                .expect("server request UUID");
            let event = errors
                .iter()
                .find(|event| event["fields"]["stage"] == "command")
                .expect("cancelled write failure recorded once");
            assert_eq!(event["target"], "domain::kernel::error");
            assert_eq!(event["fields"]["stage"], "command");
            assert_eq!(event["fields"]["kind"], "unknown");
            assert_eq!(event["fields"]["operation_id"], OPERATION);
            let span = event["spans"]
                .as_array()
                .expect("inherited spans")
                .iter()
                .find(|span| span["route"] == "/api/v1/accounts/current/affiliation")
                .expect("HTTP request context retained");
            assert_eq!(span["method"], "PUT");
            assert_eq!(span["operation_id"], OPERATION);
            Uuid::parse_str(span["request_id"].as_str().expect("request ID"))
                .expect("server request UUID");
            let secrets: Vec<String> = serde_json::from_slice(
                &std::fs::read(expected.path()).expect("private expected secrets"),
            )
            .expect("secrets");
            for secret in secrets.iter().map(String::as_str).chain([
                "13900000000",
                "Fixture Operator",
                "000000200002290010",
                "__Host-nucleus-session",
                "203.0.152.",
                fixture::PASSWORD,
                "SELECT ",
                "INSERT ",
                "UPDATE ",
                "sensitive-affiliation",
            ]) {
                assert!(
                    !events.iter().any(|event| contains_text(event, secret)),
                    "sensitive material in complete diagnostic stream"
                );
            }
            for event in &events {
                let fields = event["fields"].as_object().expect("event fields");
                for private in ["affiliation", "value", "plaintext", "body", "cookie"] {
                    assert!(!fields.contains_key(private));
                }
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("production diagnostics");
    isolated_child(|| {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let name = database(&server).await;
                let supplier = Supplier::echoing().await; let mut sms = fixture::supplier::Supplier::start().await;
                let proxy = fixture::database_proxy::Proxy::start(&server).await;
                let application = Application::start(proxy_connections(&name, &proxy), keys(0),
                    services(8).expect("settings"),
                    process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
                let (base, stop, mut task) = running(application).await;
                let actor = issued_session_for(&base, &mut sms, "13900000000").await;
                let client = http_client();
                let rejected = client.put(format!("{base}/api/v1/accounts/current/affiliation")).header("session-id", &actor.0).header("cookie", &actor.1).header("operation-id", Uuid::new_v4().to_string()).header("x-real-ip", "203.0.152.1").json(&json!({"affiliation":"sensitive-affiliation"})).send().await.expect("redacted invalid input");
                assert_eq!(rejected.status(), 422);
                let owner = connections(&server, &name, "postgres").recover().await.expect("isolated observer");
                owner.client().batch_execute("REVOKE SELECT ON center.profiles FROM clerk").await.expect("isolated query denial");
                let response = client.get(format!("{base}/api/v1/accounts/current/affiliation")).header("session-id", &actor.0).header("cookie", &actor.1).header("operation-id", READ_OPERATION).header("x-real-ip", "203.0.152.3").send().await.expect("read failure");
                assert_eq!(response.status(), 503);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure"),json!({"code":"dependency_unavailable"}));
                let response = client.get(format!("{base}/api/v1/accounts/current/affiliations")).header("session-id", &actor.0).header("cookie", &actor.1).header("operation-id", HISTORY_OPERATION).header("x-real-ip", "203.0.152.5").send().await.expect("history query failure");
                assert_eq!(response.status(),503); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("no false empty history"),json!({"code":"dependency_unavailable"}));
                let response = client.get(format!("{base}/api/v1/persons/{}/affiliations",Uuid::new_v4())).header("session-id",&actor.0).header("cookie",&actor.1).header("operation-id",MANAGEMENT_OPERATION).header("x-real-ip","203.0.152.6").send().await.expect("qualified management query failure");
                assert_eq!(response.status(),503); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("management read failure is not absence"),json!({"code":"dependency_unavailable"}));
                owner.client().batch_execute("GRANT SELECT ON center.profiles TO clerk").await.expect("restore isolated runtime privilege");
                let response = client.get(format!("{base}/api/v1/accounts/current/affiliation")).header("session-id", &actor.0).header("cookie", &actor.1).header("operation-id", Uuid::new_v4().to_string()).header("x-real-ip", "203.0.152.4").send().await.expect("read after query failure");
                assert_eq!(response.status(),404);assert_eq!(response.json::<serde_json::Value>().await.expect("still unfilled"),json!({"code":"affiliation_unset"}));
                assert_eq!(owner.client().query_one("SELECT count(*) FROM center.affiliations", &[]).await.expect("read failures preserve facts").get::<_,i64>(0),0);
                proxy.arm(fixture::database_proxy::Fault::DelayedCommit);
                let caller = tokio::spawn(client.put(format!("{base}/api/v1/accounts/current/affiliation")).header("session-id", &actor.0).header("cookie", &actor.1).header("operation-id", OPERATION).header("x-real-ip", "203.0.152.2").json(&json!({"affiliation":"01"})).send());
                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original commit response lost");
                proxy.arm(fixture::database_proxy::Fault::StalledAffiliation); proxy.release();
                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("independent recovery suspended before its fact read");
                caller.abort(); assert!(caller.await.expect_err("caller cancelled").is_cancelled());
                let row = owner.client().query_one("SELECT encode(value,'hex') FROM center.affiliations WHERE operation_id=$1::text::uuid", &[&OPERATION]).await.expect("original fact before recovery");
                std::fs::write(std::env::var_os("NUCLEUS_TEST_AFFILIATION_LOG").expect("private expected file"), serde_json::to_vec(&[actor.1.split_once('=').expect("cookie token").1.to_owned(), row.get::<_,String>(0)]).expect("expected material")).expect("private expected fixture values");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38) # 1); COMMIT").await.expect("damage only the isolated retained value");
                stop.send(()).expect("stop during accepted recovery");
                assert!(timeout(Duration::from_millis(30), &mut task).await.is_err(), "shutdown retains the accepted work");
                proxy.release();
                let (application, result) = task.await.expect("drained server"); result.expect("actual work completed before stop"); drop(application);
                assert_eq!(owner.client().query_one("SELECT count(*) FROM center.affiliations", &[]).await.expect("recovery did not write another fact").get::<_,i64>(0),1);
                owner.terminate().await.expect("observer ended"); proxy.stop().await;
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 1); assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err()); sms.stop().await;
            }).await.expect("bounded failed recovery diagnostic");
        });
    });
}

/// Count every stored value of one person through the management history read.
async fn history_count(base: &str, reader: &(String, String), person_id: Uuid) -> i64 {
    let response = http_client()
        .get(format!("{base}/api/v1/persons/{person_id}/affiliations"))
        .header("operation-id", Uuid::new_v4().to_string())
        .header("session-id", &reader.0)
        .header("cookie", &reader.1)
        .header("x-real-ip", "198.51.100.252")
        .send()
        .await
        .expect("management history");
    assert_eq!(response.status(), 200);
    response.json::<serde_json::Value>().await.expect("history")["count"]
        .as_i64()
        .expect("total count")
}

/// Count the reader's own stored values through the self history read.
async fn own_history_count(base: &str, reader: &(String, String)) -> i64 {
    let response = http_client()
        .get(format!("{base}/api/v1/accounts/current/affiliations"))
        .header("operation-id", Uuid::new_v4().to_string())
        .header("session-id", &reader.0)
        .header("cookie", &reader.1)
        .header("x-real-ip", "198.51.100.253")
        .send()
        .await
        .expect("self history");
    assert_eq!(response.status(), 200);
    response.json::<serde_json::Value>().await.expect("history")["count"]
        .as_i64()
        .expect("total count")
}
