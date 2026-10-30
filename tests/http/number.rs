use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*};

#[test]
fn workforce_number_candidate_allows_a_numbers_only_operator_to_complete_assistance() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(60), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let institution = issued_session_for(&base, &mut sms, "13900000000").await;
            let manager = issued_session_for(&base, &mut sms, "13800138000").await;
            let _target = issued_session_with_identity(&base, &mut sms, "13700000000", "198.51.100.91", "000000200002290088").await;
            let client = http_client();
            let source = std::cell::Cell::new(1_u8);
            let request = |path: &str, actor: &(String, String)| {
                let address = source.get(); source.set(address + 1);
                client.post(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.173.{address}"))
            };
            let response = request("/api/v1/roles", &institution)
                .json(&serde_json::json!({"name":"Number office","level":500,"permissions":[]})).send().await.expect("institutional role");
            assert_eq!(response.status(), 201);
            let office = response.json::<serde_json::Value>().await.expect("role")["role"].clone();
            let response = request("/api/v1/assignments/candidates", &institution)
                .json(&serde_json::json!({"mobile":"13800138000"})).send().await.expect("appointment candidate");
            assert_eq!(response.status(), 200);
            let candidate = response.json::<serde_json::Value>().await.expect("appointment confirmation");
            let response = request("/api/v1/assignments", &institution).json(&serde_json::json!({
                "target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13800138000","assignmentId":candidate["assignmentId"]},
                "role":{"id":office["id"],"changeId":office["changeId"]}
            })).send().await.expect("institutional level without permissions");
            assert_eq!(response.status(), 201);
            let response = request("/api/v1/workforce/roles", &institution)
                .json(&serde_json::json!({"entry":"institution","name":"Number manager","level":200,"permissions":["workforce.numbers"]})).send().await.expect("numbers-only role");
            assert_eq!(response.status(), 201);
            let role = response.json::<serde_json::Value>().await.expect("role")["role"].clone();
            for mobile in ["13800138000", "13700000000"] {
                let response = request("/api/v1/workforce/grants/candidates", &institution)
                    .json(&serde_json::json!({"entry":"institution","mobile":mobile})).send().await.expect("admission candidate");
                assert_eq!(response.status(), 200);
                let candidate = response.json::<serde_json::Value>().await.expect("admission confirmation");
                let role = if mobile == "13800138000" { &role } else {
                    candidate["roles"].as_array().expect("roles").iter().find(|role| role["kind"] == "full").expect("higher domain level is eligible")
                };
                let response = request("/api/v1/workforce/grants", &institution).json(&serde_json::json!({
                    "entry":"institution","target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":mobile,"assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]},
                    "role":{"id":role["id"],"changeId":role["changeId"]}
                })).send().await.expect("admit participant");
                assert_eq!(response.status(), 201);
            }
            for (path, body) in [
                ("/api/v1/accounts/candidates", serde_json::json!({"mobile":"13700000000"})),
                ("/api/v1/assignments/candidates", serde_json::json!({"mobile":"13700000000"})),
                ("/api/v1/workforce/suspensions/candidates", serde_json::json!({"mobile":"13700000000"})),
                ("/api/v1/workforce/grants/candidates", serde_json::json!({"entry":"domain","mobile":"13700000000"})),
                ("/api/v1/workforce/grants/candidates", serde_json::json!({"entry":"institution","mobile":"13700000000"})),
            ] {
                let response = request(path, &manager).json(&body).send().await.expect("unrelated permission denied");
                assert_eq!(response.status(), 403, "{path}");
            }
            let path = "/api/v1/workforce/numbers/candidates";
            let body = serde_json::json!({"mobile":"13700000000"});
            let anonymous = (Uuid::new_v4().to_string(), String::new());
            let mut cadence = tokio::time::interval(Duration::from_millis(150));
            cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            for (builder, status, code) in [
                (request(path, &anonymous).json(&body), 401, "authentication_invalid"),
                (request(path, &institution).json(&body), 403, "permission_denied"),
                (request(path, &manager).header("origin", "https://wrong.invalid").json(&body), 403, "permission_denied"),
                (request(path, &manager).header("origin", "").header("sec-fetch-site", "same-origin").json(&body), 403, "permission_denied"),
                (request(path, &manager).header("sec-fetch-site", "cross-site").json(&body), 403, "permission_denied"),
                (request(&format!("{path}?extra=1"), &manager).json(&body), 400, "invalid_input"),
                (request(path, &manager).json(&serde_json::json!({"mobile":"13700000000","extra":true})), 422, "invalid_input"),
                (request(path, &manager).json(&serde_json::json!({"mobile":"137"})), 422, "invalid_input"),
                (request(path, &manager).json(&serde_json::json!({})), 422, "invalid_input"),
                (request(path, &manager).header("content-type", "application/json").body("{"), 400, "invalid_input"),
                (request(path, &manager).body(body.to_string()), 415, "invalid_input"),
                (request(path, &manager).header("content-type", "application/json").body(" ".repeat(8193)), 413, "invalid_input"),
                (request(path, &manager).json(&serde_json::json!({"mobile":"13600000000"})), 404, "target_unavailable"),
                (request(path, &manager).json(&serde_json::json!({"mobile":"13800138000"})), 404, "target_unavailable"),
            ] {
                cadence.tick().await;
                let response = builder.send().await.expect("candidate boundary");
                assert_eq!(response.status(), status, "{code}");
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("rejection"), serde_json::json!({"code":code}));
            }
            let response = request(path, &manager).json(&body).send().await.expect("discover target");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            let candidate = response.json::<serde_json::Value>().await.expect("candidate");
            assert_eq!(candidate["code"], "ok");
            assert_eq!(candidate.as_object().expect("envelope").len(), 3);
            assert_eq!(candidate["identity"].as_object().expect("masked identity").len(), 6);
            assert_eq!(candidate["identity"]["name"], "F*");
            assert_eq!(candidate["identity"]["idNumber"], serde_json::json!({"suffix":"0088"}));
            assert_eq!(candidate["identity"]["mobile"], "13700000000");
            Uuid::parse_str(candidate["grantId"].as_str().expect("grant ID")).expect("UUID");
            let observer = connections(&server, &name, "clerk").recover().await.expect("observer");
            assert_eq!(number_history(&base, &manager, candidate["identity"]["personId"].as_str().expect("target person")).await["count"], 0, "candidate is read-only");
            let year: i32 = observer.client().query_one("SELECT EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer-1", &[]).await.expect("prior year").get(0);
            // The operator obtains every target confirmation value from this candidate alone.
            let identity = &candidate["identity"];
            let target = serde_json::json!({"accountId":identity["accountId"],"personId":identity["personId"],"revisionId":identity["revisionId"],"mobile":identity["mobile"]});
            let allocation_path = format!("/api/v1/workforce/persons/{}/numbers", identity["personId"].as_str().expect("person ID"));
            let response = request(&allocation_path, &manager).json(&serde_json::json!({"target":target,"subtype":902,"year":year,"reason":"Verified entry record"})).send().await.expect("complete assistance");
            assert_eq!(response.status(), 201);
            assert_eq!(response.json::<serde_json::Value>().await.expect("allocation")["number"]["year"], year);
            let response = request(path, &manager).json(&body).send().await.expect("already allocated");
            assert_eq!(response.status(), 409);
            assert_eq!(response.json::<serde_json::Value>().await.expect("conflict"), serde_json::json!({"code":"number_present"}));
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let route = &document["paths"][path]["post"];
            assert_eq!(route["operationId"], "workforceNumberCandidate");
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/number.Search");
            assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/number.Candidate");
            assert_eq!(document["components"]["schemas"]["number.Search"]["additionalProperties"], false);
            for status in ["200", "400", "401", "403", "404", "409", "413", "415", "422", "429", "503"] {
                assert_eq!(route["responses"][status]["headers"]["Cache-Control"]["schema"]["type"], "string", "{status}");
            }
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application, ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 3);
            assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded number candidate HTTP acceptance");
    });
}

#[test]
fn workforce_number_assistance_obeys_management_http_and_openapi_contracts() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(60), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let institution = issued_session_for(&base, &mut sms, "13900000000").await;
            let member = issued_session_for(&base, &mut sms, "13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = std::cell::Cell::new(1_u8);
            let request = |path: &str, actor: &(String,String), operation: Uuid| {
                let address = source.get(); source.set(address+1);
                client.post(format!("{base}{path}")).header("operation-id", operation.to_string()).header("session-id", &actor.0).header("cookie", &actor.1).header("origin", "https://example.com").header("x-real-ip", format!("203.0.172.{address}"))
            };
            let response = request("/api/v1/workforce/roles", &institution, Uuid::new_v4()).json(&serde_json::json!({"entry":"institution","name":"Number manager","level":500,"permissions":["workforce.numbers"]})).send().await.expect("number management role");
            assert_eq!(response.status(),201);
            let office = response.json::<serde_json::Value>().await.expect("role")["role"].clone();
            let mut target = serde_json::Value::Null;
            for (actor,entry,mobile) in [(&institution,"institution","13800138000"),(&member,"domain","13900000000")] {
                let response = request("/api/v1/workforce/grants/candidates",actor,Uuid::new_v4()).json(&serde_json::json!({"entry":entry,"mobile":mobile})).send().await.expect("candidate");
                assert_eq!(response.status(),200);
                let candidate = response.json::<serde_json::Value>().await.expect("candidate");
                let role = if entry=="institution" { candidate["roles"].as_array().expect("choices").iter().find(|role|role["kind"]=="full").expect("full role").clone() } else { office.clone() };
                let confirmation = serde_json::json!({"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":mobile});
                if entry=="institution" { target=confirmation.clone(); }
                let mut admission = confirmation; admission["assignmentId"]=candidate["assignmentId"].clone(); admission["suspensionId"]=candidate["suspensionId"].clone(); admission["grant"]=candidate["grant"].clone();
                let response = request("/api/v1/workforce/grants",actor,Uuid::new_v4()).json(&serde_json::json!({"entry":entry,"target":admission,"role":{"id":role["id"],"changeId":role["changeId"]}})).send().await.expect("real admission");
                assert_eq!(response.status(),201);
            }
            let observer = connections(&server,&name,"clerk").recover().await.expect("independent observer");
            let year:i32 = observer.client().query_one("SELECT EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer-1",&[]).await.expect("previous China Standard Time year").get(0);
            let path = format!("/api/v1/workforce/persons/{}/numbers",target["personId"].as_str().expect("person"));
            let body = serde_json::json!({"target":target,"subtype":902,"year":year,"reason":"Verified original entry record"});
            let mut stale=body.clone(); stale["target"]["revisionId"]=serde_json::json!(Uuid::new_v4());
            let mut current=body.clone(); current["year"]=serde_json::json!(year+1);
            let mut future=body.clone(); future["year"]=serde_json::json!(year+2);
            let mut invalid=body.clone(); invalid["target"]["revisionId"]=serde_json::json!("invalid");
            let mut blank=body.clone(); blank["reason"]=serde_json::json!("   ");
            let mut absent=body.clone(); absent["target"]["personId"]=serde_json::json!(Uuid::new_v4());
            let absent_path=format!("/api/v1/workforce/persons/{}/numbers",absent["target"]["personId"].as_str().expect("absent person"));
            let unauthenticated=(Uuid::new_v4().to_string(),String::new());
            let mut cadence=tokio::time::interval(Duration::from_millis(150));
            cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            for (builder,status,code) in [
                (request(&path,&unauthenticated,Uuid::new_v4()).json(&body),401,"authentication_invalid"),
                (request(&path,&institution,Uuid::new_v4()).header("origin","https://wrong.invalid").json(&body),403,"permission_denied"),
                (request(&path,&member,Uuid::new_v4()).json(&body),403,"permission_denied"),
                (request(&absent_path,&institution,Uuid::new_v4()).json(&absent),404,"target_unavailable"),
                (request(&path,&institution,Uuid::new_v4()).json(&absent),409,"target_changed"),
                (request(&path,&institution,Uuid::new_v4()).json(&stale),409,"target_changed"),
                (request(&path,&institution,Uuid::new_v4()).json(&current),422,"invalid_input"),
                (request(&path,&institution,Uuid::new_v4()).json(&future),422,"invalid_input"),
                (request(&path,&institution,Uuid::new_v4()).json(&invalid),422,"invalid_input"),
                (request(&path,&institution,Uuid::new_v4()).json(&blank),422,"invalid_input"),
                (request(&path,&institution,Uuid::new_v4()).body(body.to_string()),415,"invalid_input"),
                (request(&path,&institution,Uuid::new_v4()).header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
                (request("/api/v1/workforce/persons/invalid/numbers",&unauthenticated,Uuid::new_v4()).json(&body),400,"invalid_input"),
                (request(&format!("{path}?extra=1"),&institution,Uuid::new_v4()).json(&body),400,"invalid_input"),
            ] {
                cadence.tick().await;
                let response=builder.send().await.expect("management HTTP boundary");
                assert_eq!(response.status(),status,"{code}"); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("rejection"),serde_json::json!({"code":code}));
            }
            let operation=Uuid::new_v4();
            let response=request(&path,&institution,operation).json(&body).send().await.expect("assistance");
            assert_eq!(response.status(),201); assert!(!response.headers().contains_key("set-cookie"));
            let created=response.json::<serde_json::Value>().await.expect("number");
            let row=observer.client().query_one("SELECT revision.id,(revision.number).subtype,(revision.number).year,(revision.number).serial,allocation.source,allocation.reason,allocation.sequence,number.sequence AS state_sequence FROM workforce.revisions revision JOIN workforce.allocations allocation ON allocation.number_id=revision.id JOIN workforce.numbers number ON number.revision_id=revision.id JOIN identity.persons person ON person.id=allocation.person_id WHERE allocation.operation_id=$1",&[&operation]).await.expect("independent durable allocation before replay");
            assert_eq!(row.get::<_,&str>("source"),"assistance"); assert_eq!(row.get::<_,&str>("reason"),"Verified original entry record"); assert_eq!(row.get::<_,i64>("sequence"),row.get::<_,i64>("state_sequence"));
            assert_eq!(created,serde_json::json!({"code":"ok","number":{"id":row.get::<_,Uuid>("id"),"subtype":902,"year":year,"serial":1}}));
            let response=request(&path,&institution,operation).json(&body).send().await.expect("replay"); assert_eq!(response.status(),201); assert_eq!(response.json::<serde_json::Value>().await.expect("original"),created);
            let mut changed=body.clone(); changed["reason"]=serde_json::json!("Different evidence");
            for (operation,body,code) in [(operation,changed,"request_mismatch"),(Uuid::new_v4(),body,"number_present")] {
                let response=request(&path,&institution,operation).json(&body).send().await.expect("conflict"); assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("code"),serde_json::json!({"code":code}));
            }
            let document=client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let route=&document["paths"]["/api/v1/workforce/persons/{id}/numbers"]["post"];
            assert_eq!(route["operationId"],"assistWorkforceNumber");
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/number.Assistance");
            assert_eq!(route["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/number.Receipt");
            for status in ["400","401","403","404","409","413","415","422","429","503"] { assert!(route["responses"].get(status).is_some(),"{status}"); }
            let schema=&document["components"]["schemas"]["number.Assistance"];
            assert_eq!(schema["additionalProperties"],false);
            for field in ["subtype","year","reason","target"] { assert!(schema["required"].as_array().expect("required").contains(&serde_json::json!(field))); }
            assert_eq!(schema["properties"]["target"]["$ref"],"#/components/schemas/grant.member.Target");
            assert_eq!(number_history(&base,&institution,target["personId"].as_str().expect("target person")).await["count"],1,"once");
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application,ended)=task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);
            assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded assistance HTTP acceptance");
    });
}

#[test]
fn workforce_number_supplement_and_correction_obey_management_http_and_openapi_contracts() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(60), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let institution = issued_session_for(&base, &mut sms, "13900000000").await;
            let manager = issued_session_for(&base, &mut sms, "13800138000").await;
            let member = issued_session_with_identity(&base, &mut sms, "13700000000", "198.51.100.91", "000000200002290088").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = std::cell::Cell::new(1_u8);
            let request = |path: &str, actor: &(String,String), operation: Uuid| {
                let address = source.get(); source.set(address+1);
                client.post(format!("{base}{path}")).header("operation-id", operation.to_string()).header("session-id", &actor.0).header("cookie", &actor.1).header("origin", "https://example.com").header("x-real-ip", format!("203.0.173.{address}"))
            };
            let response = request("/api/v1/roles", &institution, Uuid::new_v4()).json(&serde_json::json!({"name":"Senior office","level":500,"permissions":[]})).send().await.expect("institution role");
            assert_eq!(response.status(), 201);
            let office = response.json::<serde_json::Value>().await.expect("office")["role"].clone();
            let response = request("/api/v1/assignments/candidates", &institution, Uuid::new_v4()).json(&serde_json::json!({"mobile":"13800138000"})).send().await.expect("appointment candidate");
            assert_eq!(response.status(), 200);
            let appointment = response.json::<serde_json::Value>().await.expect("appointment");
            let response = request("/api/v1/assignments", &institution, Uuid::new_v4()).json(&serde_json::json!({"target":{"accountId":appointment["identity"]["accountId"],"personId":appointment["identity"]["personId"],"revisionId":appointment["identity"]["revisionId"],"mobile":"13800138000","assignmentId":appointment["assignmentId"]},"role":{"id":office["id"],"changeId":office["changeId"]}})).send().await.expect("promote manager");
            assert_eq!(response.status(), 201);
            let response = request("/api/v1/workforce/roles", &institution, Uuid::new_v4()).json(&serde_json::json!({"entry":"institution","name":"Numbers only","level":200,"permissions":["workforce.numbers"]})).send().await.expect("numbers-only role");
            assert_eq!(response.status(), 201);
            let numbers_role = response.json::<serde_json::Value>().await.expect("role")["role"].clone();
            for mobile in ["13800138000", "13700000000"] {
                let response = request("/api/v1/workforce/grants/candidates", &institution, Uuid::new_v4()).json(&serde_json::json!({"entry":"institution","mobile":mobile})).send().await.expect("grant candidate");
                assert_eq!(response.status(), 200);
                let candidate = response.json::<serde_json::Value>().await.expect("candidate");
                let role = if mobile == "13800138000" { &numbers_role } else {
                    candidate["roles"].as_array().expect("choices").iter().find(|role| role["kind"] == "default").expect("default role")
                };
                let response = request("/api/v1/workforce/grants", &institution, Uuid::new_v4()).json(&serde_json::json!({"entry":"institution","target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":mobile,"assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]},"role":{"id":role["id"],"changeId":role["changeId"]}})).send().await.expect("admit participant");
                assert_eq!(response.status(), 201);
            }
            assert_eq!(request("/api/v1/workforce/numbers", &manager, Uuid::new_v4()).json(&serde_json::json!({"subtype":904})).send().await.expect("manager self-service number").status(), 201);
            // Operator with only workforce.numbers discovers the target via the candidate route.
            let response = request("/api/v1/workforce/numbers/candidates", &manager, Uuid::new_v4()).json(&serde_json::json!({"mobile":"13700000000"})).send().await.expect("discover target");
            assert_eq!(response.status(), 200);
            let discovered = response.json::<serde_json::Value>().await.expect("discovered")["identity"].clone();
            let target = serde_json::json!({"accountId":discovered["accountId"],"personId":discovered["personId"],"revisionId":discovered["revisionId"],"mobile":discovered["mobile"]});
            let candidate_path = "/api/v1/workforce/numbers/corrections/candidates";
            let search = serde_json::json!({"mobile":"13700000000"});
            let response = request(candidate_path, &manager, Uuid::new_v4()).json(&search).send().await.expect("unnumbered correction target");
            assert_eq!(response.status(), 409);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("missing number"), serde_json::json!({"code":"target_unavailable"}));
            let observer = connections(&server,&name,"clerk").recover().await.expect("independent observer");
            let year:i32 = observer.client().query_one("SELECT EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer-1",&[]).await.expect("previous China Standard Time year").get(0);
            let path = format!("/api/v1/workforce/persons/{}/supplements",target["personId"].as_str().expect("person"));
            let body = serde_json::json!({"target":target,"subtype":902,"year":year,"serial":42,"reason":"Verified original historical number"});
            let occupied_body = serde_json::json!({"target":target,"subtype":902,"year":year+1,"serial":1,"reason":"Same category pool serial"});
            let mut stale=body.clone(); stale["target"]["revisionId"]=serde_json::json!(Uuid::new_v4());
            let mut future=body.clone(); future["year"]=serde_json::json!(year+2);
            let mut zero_serial=body.clone(); zero_serial["serial"]=serde_json::json!(0);
            let mut large_serial=body.clone(); large_serial["serial"]=serde_json::json!(1000);
            let mut invalid=body.clone(); invalid["target"]["revisionId"]=serde_json::json!("invalid");
            let mut blank=body.clone(); blank["reason"]=serde_json::json!("   ");
            let mut absent=body.clone(); absent["target"]["personId"]=serde_json::json!(Uuid::new_v4());
            let absent_path=format!("/api/v1/workforce/persons/{}/supplements",absent["target"]["personId"].as_str().expect("absent person"));
            let unauthenticated=(Uuid::new_v4().to_string(),String::new());
            let mut cadence=tokio::time::interval(Duration::from_millis(150));
            cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            for (builder,status,code) in [
                (request(&path,&unauthenticated,Uuid::new_v4()).json(&body),401,"authentication_invalid"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://wrong.invalid").json(&body),403,"permission_denied"),
                (request(&path,&institution,Uuid::new_v4()).json(&body),403,"permission_denied"),
                (request(&path,&member,Uuid::new_v4()).json(&body),403,"permission_denied"),
                (request(&absent_path,&manager,Uuid::new_v4()).json(&absent),404,"target_unavailable"),
                (request(&path,&manager,Uuid::new_v4()).json(&absent),409,"target_changed"),
                (request(&path,&manager,Uuid::new_v4()).json(&stale),409,"target_changed"),
                (request(&path,&manager,Uuid::new_v4()).json(&occupied_body),409,"number_occupied"),
                (request(&path,&manager,Uuid::new_v4()).json(&future),422,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).json(&zero_serial),422,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).json(&large_serial),422,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).json(&invalid),422,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).json(&blank),422,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).body(body.to_string()),415,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
                (request("/api/v1/workforce/persons/invalid/supplements",&unauthenticated,Uuid::new_v4()).json(&body),400,"invalid_input"),
                (request(&format!("{path}?extra=1"),&manager,Uuid::new_v4()).json(&body),400,"invalid_input"),
            ] {
                cadence.tick().await;
                let response=builder.send().await.expect("supplement HTTP boundary");
                assert_eq!(response.status(),status,"{code}"); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("rejection"),serde_json::json!({"code":code}));
            }
            let operation=Uuid::new_v4();
            let response=request(&path,&manager,operation).json(&body).send().await.expect("supplement");
            assert_eq!(response.status(),201); assert!(!response.headers().contains_key("set-cookie"));
            let created=response.json::<serde_json::Value>().await.expect("number");
            let row=observer.client().query_one("SELECT revision.id,(revision.number).subtype,(revision.number).year,(revision.number).serial,allocation.source,allocation.reason,allocation.sequence,number.sequence AS state_sequence FROM workforce.revisions revision JOIN workforce.allocations allocation ON allocation.number_id=revision.id JOIN workforce.numbers number ON number.revision_id=revision.id JOIN identity.persons person ON person.id=allocation.person_id WHERE allocation.operation_id=$1",&[&operation]).await.expect("independent durable supplement before replay");
            assert_eq!(row.get::<_,&str>("source"),"supplement"); assert_eq!(row.get::<_,&str>("reason"),"Verified original historical number"); assert_eq!(row.get::<_,i64>("sequence"),row.get::<_,i64>("state_sequence"));
            assert_eq!(created,serde_json::json!({"code":"ok","number":{"id":row.get::<_,Uuid>("id"),"subtype":902,"year":year,"serial":42}}));
            let response=request(&path,&manager,operation).json(&body).send().await.expect("replay"); assert_eq!(response.status(),201); assert_eq!(response.json::<serde_json::Value>().await.expect("original"),created);
            let mut changed=body.clone(); changed["serial"]=serde_json::json!(43);
            for (operation,payload,code) in [(operation,changed,"request_mismatch"),(Uuid::new_v4(),body,"number_present")] {
                let response=request(&path,&manager,operation).json(&payload).send().await.expect("conflict"); assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("code"),serde_json::json!({"code":code}));
            }
            let document=client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let route=&document["paths"]["/api/v1/workforce/persons/{id}/supplements"]["post"];
            assert_eq!(route["operationId"],"supplementWorkforceNumber");
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/number.Supplement");
            assert_eq!(route["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/number.Receipt");
            for status in ["400","401","403","404","409","413","415","422","429","503"] { assert!(route["responses"].get(status).is_some(),"{status}"); }
            let schema=&document["components"]["schemas"]["number.Supplement"];
            assert_eq!(schema["additionalProperties"],false);
            for field in ["subtype","year","serial","reason","target"] { assert!(schema["required"].as_array().expect("required").contains(&serde_json::json!(field))); }
            assert_eq!(schema["properties"]["serial"]["minimum"],1);
            assert_eq!(schema["properties"]["serial"]["maximum"],999);
            assert_eq!(schema["properties"]["target"]["$ref"],"#/components/schemas/grant.member.Target");
            let history=number_history(&base,&manager,target["personId"].as_str().expect("target person")).await;
            assert_eq!(history["revisions"].as_array().expect("revisions").iter().filter(|revision|revision["source"]=="supplement").count(),1,"one supplement");
            for (builder, status, code) in [
                (request(candidate_path, &unauthenticated, Uuid::new_v4()).json(&search), 401, "authentication_invalid"),
                (request(candidate_path, &institution, Uuid::new_v4()).json(&search), 403, "permission_denied"),
                (request(candidate_path, &member, Uuid::new_v4()).json(&search), 403, "permission_denied"),
                (request(candidate_path, &manager, Uuid::new_v4()).header("origin", "https://wrong.invalid").json(&search), 403, "permission_denied"),
                (request(candidate_path, &manager, Uuid::new_v4()).header("origin", "").header("sec-fetch-site", "same-origin").json(&search), 403, "permission_denied"),
                (request(candidate_path, &manager, Uuid::new_v4()).header("sec-fetch-site", "cross-site").json(&search), 403, "permission_denied"),
                (request(&format!("{candidate_path}?extra=1"), &manager, Uuid::new_v4()).json(&search), 400, "invalid_input"),
                (request(candidate_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"mobile":"13700000000","extra":true})), 422, "invalid_input"),
                (request(candidate_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"mobile":"137"})), 422, "invalid_input"),
                (request(candidate_path, &manager, Uuid::new_v4()).json(&serde_json::json!({})), 422, "invalid_input"),
                (request(candidate_path, &manager, Uuid::new_v4()).header("content-type", "application/json").body("{"), 400, "invalid_input"),
                (request(candidate_path, &manager, Uuid::new_v4()).body(search.to_string()), 415, "invalid_input"),
                (request(candidate_path, &manager, Uuid::new_v4()).header("content-type", "application/json").body(" ".repeat(8193)), 413, "invalid_input"),
                (request(candidate_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"mobile":"13600000000"})), 404, "target_unavailable"),
                (request(candidate_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"mobile":"13800138000"})), 404, "target_unavailable"),
                (request("/api/v1/workforce/numbers/candidates", &manager, Uuid::new_v4()).json(&search), 409, "number_present"),
                (request("/api/v1/assignments/candidates", &manager, Uuid::new_v4()).json(&search), 403, "permission_denied"),
                (request("/api/v1/workforce/grants/candidates", &manager, Uuid::new_v4()).json(&serde_json::json!({"entry":"domain","mobile":"13700000000"})), 403, "permission_denied"),
            ] {
                cadence.tick().await;
                let response = builder.send().await.expect("correction candidate boundary");
                assert_eq!(response.status(), status, "{code}");
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("rejection"), serde_json::json!({"code":code}));
            }
            let response = request(candidate_path, &manager, Uuid::new_v4()).json(&search).send().await.expect("discover correction target");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            let candidate = response.json::<serde_json::Value>().await.expect("correction candidate");
            assert_eq!(candidate, serde_json::json!({"code":"ok","identity":{"accountId":target["accountId"],"personId":target["personId"],"revisionId":target["revisionId"],"mobile":"13700000000","name":"F*","idNumber":{"suffix":"0088"}},"number":created["number"]}));
            let route = &document["paths"][candidate_path]["post"];
            assert_eq!(route["operationId"], "workforceNumberCorrectionCandidate");
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/number.Search");
            assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/number.CorrectionCandidate");
            for status in ["200", "400", "401", "403", "404", "409", "413", "415", "422", "429", "500", "503"] {
                assert_eq!(route["responses"][status]["headers"]["Cache-Control"]["schema"]["type"], "string", "{status}");
            }
            let schema = &document["components"]["schemas"]["number.CorrectionCandidate"];
            assert_eq!(schema["properties"].as_object().expect("candidate properties").len(), 3);
            assert_eq!(schema["properties"]["identity"]["$ref"], "#/components/schemas/number.Subject");
            assert_eq!(schema["properties"]["number"]["$ref"], "#/components/schemas/number.Revision");
            let holder_path = "/api/v1/workforce/numbers/corrections/holders";
            let holder_search = serde_json::json!({"subtype":902,"year":year,"serial":42});
            for (builder, status, code) in [
                (request(holder_path, &unauthenticated, Uuid::new_v4()).json(&holder_search), 401, "authentication_invalid"),
                (request(holder_path, &institution, Uuid::new_v4()).json(&holder_search), 403, "permission_denied"),
                (request(holder_path, &member, Uuid::new_v4()).json(&holder_search), 403, "permission_denied"),
                (request(holder_path, &manager, Uuid::new_v4()).header("origin", "https://wrong.invalid").json(&holder_search), 403, "permission_denied"),
                (request(&format!("{holder_path}?extra=1"), &manager, Uuid::new_v4()).json(&holder_search), 400, "invalid_input"),
                (request(holder_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"subtype":902,"year":year,"serial":42,"extra":true})), 422, "invalid_input"),
                (request(holder_path, &manager, Uuid::new_v4()).body(holder_search.to_string()), 415, "invalid_input"),
                (request(holder_path, &manager, Uuid::new_v4()).header("content-type", "application/json").body(" ".repeat(8193)), 413, "invalid_input"),
                (request(holder_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"subtype":100,"year":1,"serial":999})), 404, "target_unavailable"),
            ] {
                cadence.tick().await;
                let response = builder.send().await.expect("correction holder boundary");
                assert_eq!(response.status(), status, "{code}");
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("rejection"), serde_json::json!({"code":code}));
            }
            let response = request(holder_path, &manager, Uuid::new_v4()).json(&holder_search).send().await.expect("current number holder");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            let holder = response.json::<serde_json::Value>().await.expect("masked holder");
            assert_eq!(holder, serde_json::json!({"code":"ok","identity":{"personId":target["personId"],"revisionId":target["revisionId"],"name":"F*"},"number":created["number"]}));
            let route = &document["paths"][holder_path]["post"];
            assert_eq!(route["operationId"], "workforceNumberCorrectionHolder");
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/number.Number");
            assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/number.CorrectionHolder");
            for status in ["200", "400", "401", "403", "404", "413", "415", "422", "429", "500", "503"] {
                assert_eq!(route["responses"][status]["headers"]["Cache-Control"]["schema"]["type"], "string", "{status}");
            }
            let schema = &document["components"]["schemas"]["number.CorrectionHolder"];
            assert_eq!(schema["properties"].as_object().expect("holder properties").len(), 3);
            assert_eq!(schema["properties"]["identity"]["$ref"], "#/components/schemas/person.revision.Identity");
            assert_eq!(schema["properties"]["number"]["$ref"], "#/components/schemas/number.Revision");
            // Every target confirmation comes from this candidate, without another read permission.
            let correction_path=format!("/api/v1/workforce/persons/{}/corrections",candidate["identity"]["personId"].as_str().expect("person"));
            let correction=serde_json::json!({"revisionId":candidate["identity"]["revisionId"],"expected":candidate["number"]["id"],"replacement":{"subtype":803,"year":year,"serial":71},"reason":"Verified category and original number"});
            let correction_operation=Uuid::new_v4();
            let response=request(&correction_path,&manager,correction_operation).json(&correction).send().await.expect("ordinary correction");
            assert_eq!(response.status(),200);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
            let corrected=response.json::<serde_json::Value>().await.expect("corrected number");
            assert_eq!(corrected["code"],"ok");assert_eq!(corrected["number"]["serial"],71);assert_eq!(corrected["number"]["subtype"],803);
            assert_ne!(corrected["number"]["id"],created["number"]["id"]);
            let response=request(holder_path,&manager,Uuid::new_v4()).json(&holder_search).send().await.expect("old number is not current occupancy");
            assert_eq!(response.status(),404);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("old number absence"),serde_json::json!({"code":"target_unavailable"}));
            let row=observer.client().query_one("SELECT correction.before_id,correction.after_id,correction.sequence,number.sequence AS state_sequence FROM workforce.corrections correction JOIN identity.persons person ON person.id=correction.person_id JOIN workforce.numbers number ON number.id=person.id AND number.revision_id=correction.after_id WHERE correction.operation_id=$1",&[&correction_operation]).await.expect("durable correction before replay");
            assert_eq!(serde_json::json!(row.get::<_,Uuid>("before_id")),created["number"]["id"]);assert_eq!(serde_json::json!(row.get::<_,Uuid>("after_id")),corrected["number"]["id"]);assert_eq!(row.get::<_,i64>("sequence"),row.get::<_,i64>("state_sequence"));
            let response=request(&correction_path,&manager,correction_operation).json(&correction).send().await.expect("correction replay");
            assert_eq!(response.status(),200);assert_eq!(response.json::<serde_json::Value>().await.expect("fixed original"),corrected);
            let mut changed=correction.clone();changed["reason"]=serde_json::json!("Other verified reason");
            let mut identical=correction.clone();identical["expected"]=corrected["number"]["id"].clone();
            let mut blank=correction.clone();blank["reason"]=serde_json::json!(" ");
            let mut occupied=identical.clone();occupied["replacement"]=serde_json::json!({"subtype":902,"year":year+1,"serial":1});
            for (builder,status,code) in [
                (request(&correction_path,&manager,correction_operation).json(&changed),409,"request_mismatch"),
                (request(&correction_path,&manager,Uuid::new_v4()).json(&correction),409,"target_changed"),
                (request(&correction_path,&manager,Uuid::new_v4()).json(&identical),422,"invalid_input"),
                (request(&correction_path,&manager,Uuid::new_v4()).json(&occupied),409,"number_occupied"),
                (request(&correction_path,&manager,Uuid::new_v4()).json(&blank),422,"invalid_input"),
                (request(&correction_path,&member,Uuid::new_v4()).json(&correction),403,"permission_denied"),
                (request(&correction_path,&unauthenticated,Uuid::new_v4()).json(&correction),401,"authentication_invalid"),
                (request(&correction_path,&manager,Uuid::new_v4()).header("origin","https://wrong.invalid").json(&correction),403,"permission_denied"),
                (request(&format!("{correction_path}?extra=1"),&manager,Uuid::new_v4()).json(&correction),400,"invalid_input"),
                (request("/api/v1/workforce/persons/invalid/corrections",&unauthenticated,Uuid::new_v4()).json(&correction),400,"invalid_input"),
                (request(&correction_path,&manager,Uuid::new_v4()).body(correction.to_string()),415,"invalid_input"),
                (request(&correction_path,&manager,Uuid::new_v4()).header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
            ] {
                cadence.tick().await;
                let response=builder.send().await.expect("correction rejection");assert_eq!(response.status(),status,"{code}");assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("code"),serde_json::json!({"code":code}));
            }
            let route=&document["paths"]["/api/v1/workforce/persons/{id}/corrections"]["post"];
            assert_eq!(route["operationId"],"correctWorkforceNumber");
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/number.Correction");
            assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/number.Receipt");
            let schema=&document["components"]["schemas"]["number.Correction"];
            assert_eq!(schema["additionalProperties"],false);
            for field in ["revisionId","expected","replacement","reason"] {assert!(schema["required"].as_array().expect("required").contains(&serde_json::json!(field)));}
            assert_eq!(schema["properties"]["replacement"]["$ref"],"#/components/schemas/number.Number");
            let history_path = format!("/api/v1/workforce/persons/{}/revisions", candidate["identity"]["personId"].as_str().expect("person"));
            let get_request = |path: &str, actor: &(String, String)| {
                let address = source.get(); source.set(address + 1);
                client.get(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &actor.0).header("cookie", &actor.1).header("origin", "https://example.com").header("x-real-ip", format!("203.0.173.{address}"))
            };
            let head_request = |path: &str, actor: &(String, String)| {
                let address = source.get(); source.set(address + 1);
                client.head(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &actor.0).header("cookie", &actor.1).header("origin", "https://example.com").header("x-real-ip", format!("203.0.173.{address}"))
            };
            let response = get_request(&history_path, &manager).send().await.expect("read number history");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert!(!response.headers().contains_key("set-cookie"));
            let history = response.json::<serde_json::Value>().await.expect("history JSON");
            assert_eq!(history["code"], "ok");
            assert_eq!(history["count"], 2);
            let revisions = history["revisions"].as_array().expect("revisions array");
            assert_eq!(revisions.len(), 2);
            assert_eq!(revisions[0]["kind"], "correction");
            assert_eq!(revisions[0]["after"]["id"], corrected["number"]["id"]);
            assert_eq!(revisions[0]["before"]["id"], created["number"]["id"]);
            assert_eq!(revisions[0]["reason"], "Verified category and original number");
            assert_eq!(revisions[0]["operator"]["name"], "F*");
            assert_eq!(revisions[1]["kind"], "allocation");
            assert_eq!(revisions[1]["source"], "supplement");
            assert_eq!(revisions[1]["number"]["id"], created["number"]["id"]);
            assert_eq!(revisions[1]["reason"], "Verified original historical number");
            assert_eq!(revisions[1]["operator"]["name"], "F*");
            assert!(revisions[0]["sequence"].as_i64().expect("seq0") > revisions[1]["sequence"].as_i64().expect("seq1"));
            for (query, count, len) in [
                ("?page=1&size=1", 2, 1),
                ("?page=2&size=1", 2, 1),
                ("?page=3&size=1", 2, 0),
                ("?page=1&size=20", 2, 2),
            ] {
                let response = get_request(&format!("{history_path}{query}"), &manager).send().await.expect("paged history");
                assert_eq!(response.status(), 200);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                let page = response.json::<serde_json::Value>().await.expect("page json");
                assert_eq!(page["code"], "ok");
                assert_eq!(page["count"], count);
                assert_eq!(page["revisions"].as_array().expect("revisions").len(), len);
            }
            let response = head_request(&history_path, &manager).send().await.expect("HEAD history");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            assert!(response.bytes().await.expect("HEAD bytes").is_empty());
            let absent_history = format!("/api/v1/workforce/persons/{}/revisions", Uuid::new_v4());
            for (builder, status, code) in [
                (get_request(&format!("{history_path}?unknown=1"), &manager), 400, "invalid_input"),
                (get_request(&format!("{history_path}?page=0"), &manager), 400, "invalid_input"),
                (get_request(&format!("{history_path}?size=0"), &manager), 400, "invalid_input"),
                (get_request(&format!("{history_path}?size=101"), &manager), 400, "invalid_input"),
                (get_request(&format!("{history_path}?page=1&page=2"), &manager), 400, "invalid_input"),
                (get_request("/api/v1/workforce/persons/invalid/revisions", &manager), 400, "invalid_input"),
                (get_request(&history_path, &member), 403, "permission_denied"),
                (get_request(&history_path, &unauthenticated), 401, "authentication_invalid"),
                (get_request(&absent_history, &manager), 404, "target_unavailable"),
                (get_request(&history_path, &manager).body("{}"), 400, "invalid_input"),
                (get_request(&history_path, &manager).body(" ".repeat(8193)), 413, "invalid_input"),
            ] {
                let response = builder.send().await.expect("history rejection");
                assert_eq!(response.status(), status, "{code}");
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code": code}));
            }
            assert_eq!(head_request(&history_path, &member).send().await.expect("HEAD denied").status(), 403);
            let history_route = &document["paths"]["/api/v1/workforce/persons/{id}/revisions"]["get"];
            assert_eq!(history_route["operationId"], "listWorkforceNumberRevisions");
            assert_eq!(history_route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/number.HistoryList");
            let list_schema = &document["components"]["schemas"]["number.HistoryList"];
            assert_eq!(list_schema["properties"]["count"]["type"], "integer");
            let fact_id = &document["components"]["schemas"]["number.history.Id"];
            assert_eq!(fact_id["type"], "string");
            assert_eq!(fact_id["format"], "uuid");
            let variants = document["components"]["schemas"]["number.HistoryItem"]["oneOf"].as_array().expect("history variants");
            assert_eq!(variants.len(), 4);
            for variant in variants {
                assert_eq!(variant["properties"]["id"]["$ref"], "#/components/schemas/number.history.Id");
            }
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application,ended)=task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),3);
            assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded supplement HTTP acceptance");
    });
}

#[test]
fn workforce_number_recovery_restores_an_occupied_number_through_http() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(90), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let institution = issued_session_for(&base, &mut sms, "13900000000").await;
            let manager = issued_session_for(&base, &mut sms, "13800138000").await;
            let beneficiary = issued_session_with_identity(&base, &mut sms, "13700000000", "198.51.100.91", "000000200002290088").await;
            issued_session_with_identity(&base, &mut sms, "13600000000", "198.51.100.93", "000000200002290061").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = std::cell::Cell::new(1_u8);
            let request = |path: &str, actor: &(String,String), operation: Uuid| {
                let address = source.get(); source.set(address+1);
                client.post(format!("{base}{path}")).header("operation-id", operation.to_string()).header("session-id", &actor.0).header("cookie", &actor.1).header("origin", "https://example.com").header("x-real-ip", format!("203.0.169.{address}"))
            };
            let mut cadence = tokio::time::interval(Duration::from_millis(150));
            cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let office = request("/api/v1/roles", &institution, Uuid::new_v4()).json(&serde_json::json!({"name":"Senior office","level":500,"permissions":[]})).send().await.expect("institution role").json::<serde_json::Value>().await.expect("office")["role"].clone();
            let appointment = request("/api/v1/assignments/candidates", &institution, Uuid::new_v4()).json(&serde_json::json!({"mobile":"13800138000"})).send().await.expect("appointment candidate").json::<serde_json::Value>().await.expect("appointment");
            assert_eq!(request("/api/v1/assignments", &institution, Uuid::new_v4()).json(&serde_json::json!({"target":{"accountId":appointment["identity"]["accountId"],"personId":appointment["identity"]["personId"],"revisionId":appointment["identity"]["revisionId"],"mobile":"13800138000","assignmentId":appointment["assignmentId"]},"role":{"id":office["id"],"changeId":office["changeId"]}})).send().await.expect("appoint manager").status(), 201);
            let numbers_role = request("/api/v1/workforce/roles", &institution, Uuid::new_v4()).json(&serde_json::json!({"entry":"institution","name":"Numbers only","level":200,"permissions":["workforce.numbers"]})).send().await.expect("numbers-only role").json::<serde_json::Value>().await.expect("role")["role"].clone();
            for mobile in ["13800138000", "13700000000", "13600000000"] {
                cadence.tick().await;
                let candidate = request("/api/v1/workforce/grants/candidates", &institution, Uuid::new_v4()).json(&serde_json::json!({"entry":"institution","mobile":mobile})).send().await.expect("grant candidate").json::<serde_json::Value>().await.expect("candidate");
                let role = if mobile == "13800138000" { numbers_role.clone() } else { candidate["roles"].as_array().expect("choices").iter().find(|role| role["kind"] == "default").expect("default role").clone() };
                let response = request("/api/v1/workforce/grants", &institution, Uuid::new_v4()).json(&serde_json::json!({"entry":"institution","target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":mobile,"assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]},"role":{"id":role["id"],"changeId":role["changeId"]}})).send().await.expect("grant");
                assert_eq!(response.status(), 201, "{mobile}");
            }
            let observer = connections(&server, &name, "clerk").recover().await.expect("observer");
            let year: i32 = observer.client().query_one("SELECT EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer-1", &[]).await.expect("previous year").get(0);
            // The beneficiary's original number is corrected away, then supplemented to the occupier.
            let mut targets = Vec::new();
            for mobile in ["13700000000", "13600000000"] {
                cadence.tick().await;
                let identity = request("/api/v1/workforce/numbers/candidates", &manager, Uuid::new_v4()).json(&serde_json::json!({"mobile":mobile})).send().await.expect("number candidate").json::<serde_json::Value>().await.expect("candidate")["identity"].clone();
                targets.push(serde_json::json!({"accountId":identity["accountId"],"personId":identity["personId"],"revisionId":identity["revisionId"],"mobile":identity["mobile"]}));
            }
            let person = |target: &serde_json::Value| target["personId"].as_str().expect("person").to_owned();
            let original = request(&format!("/api/v1/workforce/persons/{}/numbers", person(&targets[0])), &manager, Uuid::new_v4()).json(&serde_json::json!({"target":targets[0],"subtype":801,"year":year,"reason":"Verified entry record"})).send().await.expect("assisted number").json::<serde_json::Value>().await.expect("original")["number"].clone();
            cadence.tick().await;
            let temporary = request(&format!("/api/v1/workforce/persons/{}/corrections", person(&targets[0])), &manager, Uuid::new_v4()).json(&serde_json::json!({"revisionId":targets[0]["revisionId"],"expected":original["id"],"replacement":{"subtype":902,"year":year,"serial":42},"reason":"Temporary number"})).send().await.expect("correction").json::<serde_json::Value>().await.expect("temporary")["number"].clone();
            cadence.tick().await;
            let occupied = request(&format!("/api/v1/workforce/persons/{}/supplements", person(&targets[1])), &manager, Uuid::new_v4()).json(&serde_json::json!({"target":targets[1],"subtype":801,"year":year,"serial":original["serial"],"reason":"Recorded original number"})).send().await.expect("supplement").json::<serde_json::Value>().await.expect("occupied")["number"].clone();

            // Both confirmations come from numbers-only candidate routes.
            cadence.tick().await;
            let response = request("/api/v1/workforce/recoveries/candidates", &manager, Uuid::new_v4()).json(&serde_json::json!({"mobile":"13700000000"})).send().await.expect("recovery candidate");
            assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let candidate = response.json::<serde_json::Value>().await.expect("candidate");
            assert_eq!(candidate["number"], temporary);
            assert_eq!(candidate.as_object().expect("candidate").len(), 3);
            cadence.tick().await;
            let holder = request("/api/v1/workforce/numbers/corrections/holders", &manager, Uuid::new_v4()).json(&serde_json::json!({"subtype":801,"year":year,"serial":original["serial"]})).send().await.expect("holder").json::<serde_json::Value>().await.expect("holder");
            let body = serde_json::json!({
                "beneficiary": {"personId": candidate["identity"]["personId"], "revisionId": candidate["identity"]["revisionId"], "current": candidate["number"]["id"], "origin": original["id"]},
                "occupier": {"personId": holder["identity"]["personId"], "revisionId": holder["identity"]["revisionId"], "current": holder["number"]["id"]},
                "reason": "Verified original holder record",
            });
            let path = "/api/v1/workforce/recoveries";
            let unauthenticated = (Uuid::new_v4().to_string(), String::new());
            let mut same = body.clone(); same["occupier"]["personId"] = body["beneficiary"]["personId"].clone();
            let mut stale = body.clone(); stale["occupier"]["current"] = serde_json::json!(Uuid::new_v4());
            let mut unknown = body.clone(); unknown["extra"] = serde_json::json!(true);
            let mut missing = body.clone(); missing["beneficiary"].as_object_mut().expect("beneficiary").remove("current");
            for (builder, status, code) in [
                (request(path, &unauthenticated, Uuid::new_v4()).json(&body), 401, "authentication_invalid"),
                (request(path, &beneficiary, Uuid::new_v4()).json(&body), 403, "permission_denied"),
                (request(path, &manager, Uuid::new_v4()).header("origin", "https://wrong.invalid").json(&body), 403, "permission_denied"),
                (request(&format!("{path}?extra=1"), &manager, Uuid::new_v4()).json(&body), 400, "invalid_input"),
                (request(path, &manager, Uuid::new_v4()).json(&unknown), 422, "invalid_input"),
                (request(path, &manager, Uuid::new_v4()).json(&missing), 422, "invalid_input"),
                (request(path, &manager, Uuid::new_v4()).json(&same), 422, "invalid_input"),
                (request(path, &manager, Uuid::new_v4()).json(&stale), 409, "target_changed"),
            ] {
                cadence.tick().await;
                let response = builder.send().await.expect("recovery boundary");
                assert_eq!(response.status(), status, "{code}"); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("rejection"), serde_json::json!({"code": code}));
            }
            let operation = Uuid::new_v4();
            cadence.tick().await;
            let response = request(path, &manager, operation).json(&body).send().await.expect("recovery");
            assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let restored = response.json::<serde_json::Value>().await.expect("restored");
            let recovery = &restored["recovery"];
            assert_eq!(recovery["personId"], body["beneficiary"]["personId"]);
            assert_eq!(recovery["origin"], original["id"]);
            assert_eq!((recovery["number"]["subtype"].clone(), recovery["number"]["year"].clone(), recovery["number"]["serial"].clone()), (original["subtype"].clone(), original["year"].clone(), original["serial"].clone()));
            assert_eq!(recovery["revocations"], serde_json::json!([
                {"id": recovery["revocations"][0]["id"], "personId": body["occupier"]["personId"], "number": occupied},
                {"id": recovery["revocations"][1]["id"], "personId": body["beneficiary"]["personId"], "number": temporary},
            ]));
            let response = request(path, &manager, operation).json(&body).send().await.expect("replay");
            assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("original"), restored);
            let mut changed = body.clone(); changed["reason"] = serde_json::json!("Another record");
            cadence.tick().await;
            let response = request(path, &manager, operation).json(&changed).send().await.expect("mismatch");
            assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":"request_mismatch"}));
            cadence.tick().await;
            let response = client.get(format!("{base}/api/v1/workforce/persons/{}/revisions", person(&targets[1]))).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &manager.0).header("cookie", &manager.1).header("origin", "https://example.com").header("x-real-ip", "203.0.169.250").send().await.expect("occupier history");
            let status = response.status();
            let history = response.json::<serde_json::Value>().await.expect("history");
            assert_eq!(status, 200, "{history}");
            assert_eq!(history["revisions"][0]["kind"], "revocation");
            assert_eq!(history["revisions"][0]["recoveryId"], recovery["id"]);
            // The revoked occupier corrects its latest revision to a value another person holds, without occupancy.
            cadence.tick().await;
            let response = request(&format!("/api/v1/workforce/persons/{}/corrections", person(&targets[1])), &manager, Uuid::new_v4())
                .json(&serde_json::json!({"revisionId":holder["identity"]["revisionId"],"expected":occupied["id"],"replacement":{"subtype":802,"year":recovery["number"]["year"],"serial":recovery["number"]["serial"]},"reason":"Corrected historical record"}))
                .send().await.expect("detached correction");
            assert_eq!(response.status(), 200);
            let detached = response.json::<serde_json::Value>().await.expect("detached");
            assert_eq!(detached["number"]["serial"], recovery["number"]["serial"]);
            cadence.tick().await;
            let holder_again = request("/api/v1/workforce/numbers/corrections/holders", &manager, Uuid::new_v4()).json(&serde_json::json!({"subtype":recovery["number"]["subtype"],"year":year,"serial":recovery["number"]["serial"]})).send().await.expect("holder").json::<serde_json::Value>().await.expect("holder");
            assert_eq!(holder_again["identity"]["personId"], body["beneficiary"]["personId"], "the beneficiary keeps the occupancy");

            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let route = &document["paths"][path]["post"];
            assert_eq!(route["operationId"], "recoverWorkforceNumber");
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/number.recovery.Recovery");
            for status in ["200", "400", "401", "403", "404", "409", "413", "415", "422", "429", "503"] { assert!(route["responses"].get(status).is_some(), "{status}"); }
            assert_eq!(document["paths"]["/api/v1/workforce/recoveries/candidates"]["post"]["operationId"], "workforceRecoveryCandidate");
            let beneficiary_schema = &document["components"]["schemas"]["number.recovery.Beneficiary"];
            assert_eq!(beneficiary_schema["additionalProperties"], false);
            for field in ["personId", "revisionId", "current", "origin"] { assert!(beneficiary_schema["required"].as_array().expect("required").contains(&serde_json::json!(field)), "{field}"); }
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application, result) = task.await.expect("server task"); result.expect("shutdown"); drop(application); supplier.task.abort(); sms.stop().await;
        }).await.expect("bounded recovery HTTP acceptance");
    });
}

#[test]
fn workforce_number_allocation_returns_the_fixed_revision_through_typed_http() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(40), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let manager = issued_session_for(&base, &mut sms, "13900000000").await;
            let member = issued_session_for(&base, &mut sms, "13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = std::cell::Cell::new(1_u8);
            let request = |method: reqwest::Method, path: &str, actor: &(String, String), operation: Uuid| {
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", operation.to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.171.{address}"))
            };
            let response = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("real candidate");
            assert_eq!(response.status(), 200);
            let candidate = response.json::<serde_json::Value>().await.expect("confirmation");
            let roles = candidate["roles"].as_array().expect("choices");
            let full = roles.iter().find(|role| role["kind"] == "full").expect("full role").clone();
            let admission = serde_json::json!({"entry":"institution","target":{
                "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
            },"role":{"id":full["id"],"changeId":full["changeId"]}});
            assert_eq!(request(reqwest::Method::POST, "/api/v1/workforce/grants", &manager, Uuid::new_v4()).json(&admission).send().await.expect("real admission").status(), 201);
            let path = "/api/v1/workforce/numbers";
            let body = serde_json::json!({"subtype":100});
            let response = request(reqwest::Method::POST, path, &manager, Uuid::new_v4()).json(&body).send().await.expect("manager without a grant");
            assert_eq!(response.status(), 403);
            assert_eq!(response.json::<serde_json::Value>().await.expect("denial"), serde_json::json!({"code":"permission_denied"}));
            let response = request(reqwest::Method::GET, "/api/v1/workforce/current/numbers", &member, Uuid::new_v4()).send().await.expect("read before allocation");
            assert_eq!(response.status(), 200);
            assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("empty value"), serde_json::json!({"code":"ok","number":null}));
            let operation = Uuid::new_v4();
            let response = request(reqwest::Method::POST, path, &member, operation).json(&body).send().await.expect("first allocation");
            assert_eq!(response.status(), 201);
            assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let created = response.json::<serde_json::Value>().await.expect("fixed revision");
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
            let row = observer.client().query_one("SELECT revision.id, (revision.number).subtype, (revision.number).year, (revision.number).serial FROM workforce.numbers number JOIN workforce.revisions revision ON revision.id = number.revision_id", &[]).await.expect("one permanent number");
            let year: i32 = observer.client().query_one("SELECT EXTRACT(YEAR FROM now() AT TIME ZONE 'Asia/Shanghai')::integer", &[]).await.expect("China Standard Time calendar year").get(0);
            assert_eq!(row.get::<_, i16>("serial"), 1);
            assert_eq!(row.get::<_, i32>("year"), year);
            assert_eq!(created, serde_json::json!({"code":"ok","number":{"id":row.get::<_,Uuid>("id"),"subtype":row.get::<_,i16>("subtype"),"year":year,"serial":1}}));
            let response = request(reqwest::Method::GET, "/api/v1/workforce/current/numbers", &member, Uuid::new_v4()).send().await.expect("current read");
            assert_eq!(response.status(), 200);
            assert_eq!(response.json::<serde_json::Value>().await.expect("current number")["number"], created["number"]);
            let response = request(reqwest::Method::HEAD, "/api/v1/workforce/current/numbers", &member, Uuid::new_v4()).send().await.expect("metadata read");
            assert_eq!(response.status(), 200);
            assert!(response.bytes().await.expect("no body").is_empty());
            assert_eq!(request(reqwest::Method::GET, "/api/v1/workforce/current/numbers?page=1", &member, Uuid::new_v4()).send().await.expect("query").status(), 400);
            assert_eq!(request(reqwest::Method::GET, "/api/v1/workforce/current/numbers", &member, Uuid::new_v4()).body("{}").send().await.expect("body").status(), 400);
            let response = request(reqwest::Method::POST, path, &member, operation).json(&body).send().await.expect("replay");
            assert_eq!(response.status(), 201);
            assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("original revision"), created);
            let response = request(reqwest::Method::POST, path, &member, operation).json(&serde_json::json!({"subtype":200})).send().await.expect("changed replay");
            assert_eq!(response.status(), 409);
            assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch"), serde_json::json!({"code":"request_mismatch"}));
            let response = request(reqwest::Method::POST, path, &member, Uuid::new_v4()).json(&body).send().await.expect("second allocation");
            assert_eq!(response.status(), 409);
            assert_eq!(response.json::<serde_json::Value>().await.expect("present"), serde_json::json!({"code":"number_present"}));
            for value in [serde_json::json!({"subtype":101}), serde_json::json!({"subtype":"100"}), serde_json::json!({}), serde_json::json!({"subtype":100,"year":2020})] {
                let response = request(reqwest::Method::POST, path, &member, Uuid::new_v4()).json(&value).send().await.expect("rejected input");
                assert_eq!(response.status(), 422);
                assert_eq!(response.json::<serde_json::Value>().await.expect("stable code"), serde_json::json!({"code":"invalid_input"}));
            }
            assert_eq!(request(reqwest::Method::POST, path, &member, Uuid::new_v4()).header("origin", "https://wrong.invalid").json(&body).send().await.expect("wrong Origin").status(), 403);
            assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.revisions", &[]).await.expect("rejections write nothing").get::<_, i64>(0), 1);
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let route = &document["paths"][path]["post"];
            assert_eq!(route["operationId"], "allocateWorkforceNumber");
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/number.Input");
            assert_eq!(route["responses"]["201"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/number.Receipt");
            let route = &document["paths"]["/api/v1/workforce/current/numbers"]["get"];
            assert_eq!(route["operationId"], "currentWorkforceNumber");
            assert!(document["components"]["schemas"]["number.Current"]["required"].as_array().expect("required fields").contains(&serde_json::json!("number")), "number is present even when null");
            assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/number.Current");
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application, ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
            assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded number allocation HTTP acceptance");
    });
}

/// Read one person's complete Workforce number history through the management read.
async fn number_history(base: &str, reader: &(String, String), person: &str) -> serde_json::Value {
    let response = http_client()
        .get(format!(
            "{base}/api/v1/workforce/persons/{person}/revisions?size=100"
        ))
        .header("operation-id", Uuid::new_v4().to_string())
        .header("session-id", &reader.0)
        .header("cookie", &reader.1)
        .header("x-real-ip", "198.51.100.254")
        .send()
        .await
        .expect("number history");
    assert_eq!(response.status(), 200);
    response.json().await.expect("history")
}
