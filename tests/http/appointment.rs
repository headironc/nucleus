use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, role::*, supplier::*};

#[test]
fn institutional_assignments_confirm_real_targets_and_preserve_original_http_results() {
    async fn history(client: &tokio_postgres::Client) -> Vec<serde_json::Value> {
        let rows = client.query(
            r#"SELECT appointment.id, appointment.person_id, appointment.revision_id, operator.person_id AS operator_id, appointment.operator_id AS operator_revision_id, appointment.sequence, appointment.assigned_at, appointment.operation_id, appointment.role_id, definition.id AS change_id, definition.name, definition.level, ARRAY(SELECT capability_id FROM access.entries WHERE change_id = definition.id ORDER BY capability_id COLLATE "C") AS permissions FROM access.appointments appointment JOIN identity.revisions operator ON operator.id = appointment.operator_id JOIN access.changes definition ON definition.id = appointment.change_id AND definition.role_id = appointment.role_id ORDER BY appointment.assigned_at DESC, appointment.id DESC"#,
            &[],
        ).await.expect("independent permanent appointment facts");
        rows.iter()
            .map(|row| {
                let level: i16 = row.get("level");
                let kind = match level {
                    1 => "default",
                    1000 => "full",
                    _ => "custom",
                };
                serde_json::json!({
                    "id":row.get::<_,Uuid>("id"),
                    "subject":{"personId":row.get::<_,Uuid>("person_id"),"revisionId":row.get::<_,Uuid>("revision_id"),"name":"F*"},
                    "operator":{"personId":row.get::<_,Uuid>("operator_id"),"revisionId":row.get::<_,Uuid>("operator_revision_id"),"name":"F*"},
                    "role":{"id":row.get::<_,Uuid>("role_id"),"kind":kind,"name":row.get::<_,String>("name"),"level":level,"permissions":row.get::<_,Vec<String>>("permissions"),"changeId":row.get::<_,Uuid>("change_id")},
                    "sequence":row.get::<_,i64>("sequence"),
                    "assignedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("assigned_at"),
                    "operationId":row.get::<_,Uuid>("operation_id")
                })
            })
            .collect()
    }

    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(45), async {
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
            let ordinary = issued_session_for(&base, &mut sms, "13800138000").await;
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent runtime observer");
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let request = |method, path: &str, identity: Option<&(String, String)>, operation: Uuid, source: u8| {
                let mut request = client.request(method, format!("{base}{path}"))
                    .header("operation-id", operation.to_string()).header("x-real-ip", format!("203.0.117.{source}"));
                if let Some(identity) = identity {
                    request = request.header("session-id", &identity.0).header("cookie", &identity.1);
                }
                request
            };
            let read = |path: &str, identity: &(String,String), source| request(reqwest::Method::GET,path,Some(identity),Uuid::new_v4(),source).header("sec-fetch-site","same-origin");
            let initial = history(observer.client()).await;
            assert_eq!(initial.len(),2,"initialization and real R2 each retain their first appointment");
            let administrator = initial.iter().find(|entry| entry["role"]["kind"]=="full").expect("initial administrator").clone();
            let registered = initial.iter().find(|entry| entry["role"]["kind"]=="default").expect("initial registered account").clone();
            for entry in &initial {
                assert_eq!(entry["sequence"],1);
                assert_eq!(entry["subject"],entry["operator"]);
            }
            let response=read("/api/v1/assignments",&manager,40).send().await.expect("complete first-appointment history");
            assert_eq!(response.status(),200);
            assert_eq!(response.headers()["cache-control"],"no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("fixed first identities and definitions"),serde_json::json!({"code":"ok","count":2,"assignments":initial}));
            for (builder,status,code) in [
                (request(reqwest::Method::GET,"/api/v1/assignments",None,Uuid::new_v4(),41).header("sec-fetch-site","same-origin"),401,"authentication_invalid"),
                (request(reqwest::Method::GET,"/api/v1/assignments",Some(&manager),Uuid::new_v4(),42),403,"permission_denied"),
                (read("/api/v1/assignments",&ordinary,43),403,"permission_denied"),
                (read("/api/v1/assignments",&manager,44).header("origin","https://wrong.invalid"),403,"permission_denied"),
            ] {
                let response=builder.send().await.expect("history identity and origin boundary");
                assert_eq!(response.status(),status);
                assert_eq!(response.headers()["cache-control"],"no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("history rejection"),serde_json::json!({"code":code}));
            }
            let response = read("/api/v1/assignments/roles", &manager, 1).send().await.expect("initial assignable list");
            assert_eq!(response.status(), 200);
            let defaults = response.json::<serde_json::Value>().await.expect("default only");
            assert_eq!(defaults["count"], 1);
            let default = defaults["roles"][0].clone();
            assert_eq!(default["kind"], "default");
            let mut definitions = Vec::new();
            for (index, input) in [
                serde_json::json!({"name":"Assignment manager","level":300,"permissions":["institution.assignments"]}),
                serde_json::json!({"name":"Available","level":200,"permissions":[]}),
                serde_json::json!({"name":"Unavailable permission","level":200,"permissions":["institution.accounts"]}),
                serde_json::json!({"name":"Unavailable level","level":300,"permissions":[]}),
            ].iter().enumerate() {
                let response=request(reqwest::Method::POST,"/api/v1/roles",Some(&manager),Uuid::new_v4(),u8::try_from(index+2).expect("source"))
                    .header("origin","https://example.com").json(input).send().await.expect("complete real role");
                assert_eq!(response.status(),201);
                definitions.push(response.json::<serde_json::Value>().await.expect("role")["role"].clone());
            }
            // Initial and online appointment families may retain the same UUID.
            let operation:Uuid = administrator["operationId"].as_str().expect("initial operation").parse().expect("UUID");
            let response=request(reqwest::Method::POST,"/api/v1/assignments/candidates",Some(&manager),operation,6)
                .header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800138000"})).send().await.expect("exact candidate");
            assert_eq!(response.status(),200); assert_eq!(response.headers()["cache-control"],"no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            let candidate=response.json::<serde_json::Value>().await.expect("masked candidate");
            assert_eq!(candidate.as_object().expect("candidate shape").len(),4);
            assert_eq!(candidate["identity"].as_object().expect("identity shape").len(),5);
            assert_eq!(candidate["identity"]["name"],"F*");
            assert_eq!(candidate["identity"]["idNumber"],serde_json::json!({"suffix":"007X"}));
            assert_eq!(candidate["role"],default);
            let session:Uuid=ordinary.0.parse().expect("session UUID");
            let row=observer.client().query_one("SELECT account.id,person.id AS person_id,person.revision_id,holder.assignment_id,person.sequence AS person_sequence FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id JOIN access.holders holder ON holder.id=account.id WHERE session.id=$1", &[&session]).await.expect("independent candidate facts");
            assert_eq!(candidate["identity"]["accountId"],serde_json::json!(row.get::<_,Uuid>("id")));
            assert_eq!(candidate["identity"]["personId"],serde_json::json!(row.get::<_,Uuid>("person_id")));
            assert_eq!(candidate["identity"]["revisionId"],serde_json::json!(row.get::<_,Uuid>("revision_id")));
            assert_eq!(candidate["assignmentId"],serde_json::json!(row.get::<_,Uuid>("assignment_id")));
            assert_eq!(row.get::<_,i64>("person_sequence"),1);
            let input=serde_json::json!({"target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13800138000","assignmentId":candidate["assignmentId"]},"role":{"id":definitions[0]["id"],"changeId":definitions[0]["changeId"]}});
            let mut invalid=input.clone(); invalid["target"]["personId"]=serde_json::json!("not-a-uuid");
            let mut unknown=input.clone(); unknown["target"]["extra"]=serde_json::json!(true);
            // Pace this protocol matrix below the real process write refill rate.
            let mut cadence=tokio::time::interval_at(tokio::time::Instant::now()+Duration::from_millis(120),Duration::from_millis(120));
            for (builder,status,code) in [
                (request(reqwest::Method::POST,"/api/v1/assignments",None,operation,7).header("origin","https://example.com").json(&invalid),401,"authentication_invalid"),
                (request(reqwest::Method::POST,"/api/v1/assignments",Some(&manager),operation,8).header("sec-fetch-site","same-origin").json(&invalid),403,"permission_denied"),
                (request(reqwest::Method::POST,"/api/v1/assignments",Some(&ordinary),operation,9).header("origin","https://example.com").json(&invalid),422,"invalid_input"),
                (request(reqwest::Method::POST,"/api/v1/assignments",Some(&ordinary),operation,10).header("origin","https://example.com").json(&input),403,"permission_denied"),
                (request(reqwest::Method::POST,"/api/v1/assignments",Some(&manager),operation,11).header("origin","https://example.com").header("authorization","Bearer fictional").json(&input),400,"invalid_input"),
                (request(reqwest::Method::POST,"/api/v1/assignments",Some(&manager),operation,12).header("origin","https://example.com").body("{}"),415,"invalid_input"),
                (request(reqwest::Method::POST,"/api/v1/assignments",Some(&manager),operation,13).header("origin","https://example.com").header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
                (request(reqwest::Method::POST,"/api/v1/assignments/candidates",Some(&manager),operation,14).header("origin","https://example.com").json(&serde_json::json!({"mobile":"138","extra":1})),422,"invalid_input"),
                (request(reqwest::Method::POST,"/api/v1/assignments/candidates",Some(&manager),operation,15).header("sec-fetch-site","same-origin").json(&serde_json::json!({"mobile":"13800138000"})),403,"permission_denied"),
                (request(reqwest::Method::POST,"/api/v1/assignments/candidates",Some(&ordinary),operation,16).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800138000"})),403,"permission_denied"),
                (request(reqwest::Method::POST,"/api/v1/assignments/candidates",Some(&manager),operation,17).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13900000000"})),404,"target_unavailable"),
                (request(reqwest::Method::POST,"/api/v1/assignments/candidates",Some(&manager),operation,18).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13700000000"})),404,"target_unavailable"),
                (request(reqwest::Method::POST,"/api/v1/assignments/candidates",Some(&manager),operation,31).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800138000","extra":1})),422,"invalid_input"),
                (request(reqwest::Method::POST,"/api/v1/assignments",Some(&manager),operation,32).header("origin","https://example.com").json(&unknown),422,"invalid_input"),
            ] {
                cadence.tick().await;
                let response=builder.send().await.expect("ordered rejection");
                assert_eq!(response.status(),status); assert_eq!(response.headers()["cache-control"],"no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure"),serde_json::json!({"code":code}));
            }
            assert_eq!(read(&format!("/api/v1/assignments?personId={}",row.get::<_,Uuid>("person_id")),&manager,49).send().await.expect("person history").json::<serde_json::Value>().await.expect("history")["count"],1,"reads and rejects do not append history");
            cadence.tick().await;
            let response=request(reqwest::Method::POST,"/api/v1/assignments",Some(&manager),operation,19).header("origin","https://example.com").json(&input).send().await.expect("actual replacement");
            assert_eq!(response.status(),201); assert!(!response.headers().contains_key("set-cookie"));
            let assigned=response.json::<serde_json::Value>().await.expect("permanent result");
            assert_eq!(assigned["code"],"ok");
            let assignment=&assigned["assignment"];
            assert_eq!(assignment.as_object().expect("assignment shape").len(),7);
            assert_eq!(assignment["role"],definitions[0]); assert_eq!(assignment["sequence"],2);
            assert_eq!(assignment["operationId"],serde_json::json!(operation));
            for identity in ["subject","operator"] {
                assert_eq!(assignment[identity].as_object().expect("fixed masked identity").len(),3);
                assert_eq!(assignment[identity]["name"],"F*");
            }
            assert_eq!(assignment["subject"]["personId"],candidate["identity"]["personId"]);
            let id:Uuid=assignment["id"].as_str().expect("assignment ID").parse().expect("UUID");
            let row=observer.client().query_one("SELECT appointment.operation_id,appointment.sequence,appointment.assigned_at,octet_length(appointment.seal),appointment.seal_key_id,person.sequence AS person_sequence,holder.assignment_id,holder.role_id,(SELECT count(*) FROM access.appointments WHERE person_id=appointment.person_id),appointment.change_id,holder.sequence,completion.action='assign' AND completion.target=appointment.holder_id::text AND completion.subject='session' AND completion.person_id=operator.person_id AND completion.revision_id=appointment.operator_id AND completion.created_at=appointment.assigned_at AND completion.expires_at IS NULL AND octet_length(completion.input)>0 AND octet_length(completion.seal)=32 AND completion.seal_key_id=0 AND completion.seal_version=1 AS completed,(SELECT count(*) FROM identity.changes WHERE person_id=appointment.person_id)=1 AS unchanged_person FROM access.appointments appointment JOIN identity.persons person ON person.id=appointment.person_id JOIN identity.revisions operator ON operator.id=appointment.operator_id JOIN access.holders holder ON holder.id=appointment.holder_id JOIN access.completions completion ON completion.family='institution_appointment' AND completion.operation_id=appointment.operation_id WHERE appointment.id=$1",&[&id]).await.expect("committed facts before replay");
            assert_eq!(row.get::<_,Uuid>(0),operation); assert_eq!(row.get::<_,i64>(1),2);
            assert_eq!(assignment["assignedAt"],serde_json::json!(row.get::<_,chrono::DateTime<chrono::Utc>>(2)));
            assert_eq!(row.get::<_,i32>(3),32); assert_eq!(row.get::<_,i64>(4),0); assert_eq!(row.get::<_,i64>(5),1);
            assert_eq!(row.get::<_,Uuid>(6),id); assert_eq!(assignment["role"]["id"],serde_json::json!(row.get::<_,Uuid>(7))); assert_eq!(row.get::<_,i64>(8),2);
            assert_eq!(assignment["role"]["changeId"],serde_json::json!(row.get::<_,Uuid>(9))); assert_eq!(row.get::<_,i64>(10),2);
            assert!(row.get::<_,bool>("completed")); assert!(row.get::<_,bool>("unchanged_person"));
            assert_eq!(read("/api/v1/roles",&ordinary,20).send().await.expect("assignment permission does not grant maintenance").status(),403);
            let permanent = history(observer.client()).await;
            assert_eq!(permanent.len(),3);
            assert_eq!(permanent[0],assigned["assignment"]);
            let response=request(reqwest::Method::GET,"/api/v1/assignments",Some(&ordinary),operation,45).header("sec-fetch-site","same-origin").send().await.expect("assignment-only authority reads every level");
            assert_eq!(response.status(),200);
            assert_eq!(response.headers()["cache-control"],"no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("read Operation-Id is not a filter"),serde_json::json!({"code":"ok","count":3,"assignments":permanent}));
            let person = registered["subject"]["personId"].as_str().expect("registered person");
            let mut personal:Vec<_> = permanent.iter().filter(|entry| entry["subject"]["personId"]==person).cloned().collect();
            personal.sort_by(|left,right| right["sequence"].as_i64().expect("sequence").cmp(&left["sequence"].as_i64().expect("sequence")));
            assert_eq!(personal.len(),2);
            let same_operation:Vec<_> = permanent.iter().filter(|entry| entry["operationId"]==serde_json::json!(operation)).cloned().collect();
            assert_eq!(same_operation.len(),2,"operation filter spans initialization and online families");
            for (query,count,expected) in [
                (format!("?personId={person}"),2,serde_json::json!(personal)),
                (format!("?personId={}&size=100",Uuid::parse_str(person).expect("person UUID").simple().to_string().to_uppercase()),2,serde_json::json!(personal)),
                (format!("?operationId={operation}"),2,serde_json::json!(same_operation)),
                (format!("?personId={person}&operationId={operation}"),1,serde_json::json!([permanent[0]])),
                (format!("?personId={person}&size=1&page=2"),2,serde_json::json!([registered])),
                (format!("?personId={person}&size=1&page=3"),2,serde_json::json!([])),
                ("?size=1&page=2".to_owned(),3,serde_json::json!([permanent[1]])),
                (format!("?personId={}",Uuid::new_v4()),0,serde_json::json!([])),
                (format!("?operationId={}",Uuid::new_v4()),0,serde_json::json!([])),
            ] {
                let response=read(&format!("/api/v1/assignments{query}"),&ordinary,46).send().await.expect("filtered history page");
                assert_eq!(response.status(),200);
                assert_eq!(response.json::<serde_json::Value>().await.expect("count and complete page"),serde_json::json!({"code":"ok","count":count,"assignments":expected}));
            }
            let invalid_queries=["?page=0","?size=101","?page=1&page=2","?personId=invalid","?operationId=invalid","?unknown=1","?page=9223372036854775807&size=100"].map(str::to_owned);
            for query in invalid_queries.into_iter().chain([format!("?personId={person}&personId={person}"),format!("?operationId={operation}&operationId={operation}")]) {
                let response=read(&format!("/api/v1/assignments{query}"),&ordinary,47).send().await.expect("strict history query");
                assert_eq!(response.status(),400);
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe history query failure"),serde_json::json!({"code":"invalid_input"}));
            }
            let response=request(reqwest::Method::HEAD,"/api/v1/assignments",Some(&ordinary),operation,48).header("sec-fetch-site","same-origin").send().await.expect("history HEAD");
            assert_eq!(response.status(),200);
            assert_eq!(response.headers()["cache-control"],"no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            assert!(response.bytes().await.expect("empty history HEAD").is_empty());
            let response=read("/api/v1/assignments/roles",&ordinary,21).send().await.expect("complete eligible definitions");
            assert_eq!(response.status(),200);
            assert_eq!(response.json::<serde_json::Value>().await.expect("strict level and permission subset"),serde_json::json!({"code":"ok","count":2,"roles":[definitions[1],default]}));
            for (query,count,roles) in [("?size=1&page=2",2,serde_json::json!([default])),("?page=3&size=1",2,serde_json::json!([]))] {
                let response=read(&format!("/api/v1/assignments/roles{query}"),&ordinary,22).send().await.expect("pagination");
                assert_eq!(response.status(),200); assert_eq!(response.json::<serde_json::Value>().await.expect("page"),serde_json::json!({"code":"ok","count":count,"roles":roles}));
            }
            for query in ["?page=0","?size=101","?page=1&page=2","?operationId=invalid","?page=9223372036854775807&size=100"] {
                let response=read(&format!("/api/v1/assignments/roles{query}"),&ordinary,23).send().await.expect("strict query");
                assert_eq!(response.status(),400); assert_eq!(response.json::<serde_json::Value>().await.expect("safe query failure"),serde_json::json!({"code":"invalid_input"}));
            }
            let response=request(reqwest::Method::HEAD,"/api/v1/assignments/roles",Some(&ordinary),operation,24).header("sec-fetch-site","same-origin").send().await.expect("HEAD");
            assert_eq!(response.status(),200); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(response.bytes().await.expect("no body").is_empty());
            assert_eq!(read("/api/v1/assignments/roles",&ordinary,25).header("origin","https://wrong.invalid").send().await.expect("GET same origin").status(),403);
            cadence.tick().await;
            let mut equivalent=input.clone();
            for (section,field) in [("target","accountId"),("target","personId"),("target","revisionId"),("target","assignmentId"),("role","id"),("role","changeId")] {
                equivalent[section][field]=serde_json::json!(Uuid::parse_str(input[section][field].as_str().expect("input ID")).expect("UUID").simple().to_string().to_uppercase());
            }
            let response=request(reqwest::Method::POST,"/api/v1/assignments",Some(&manager),operation,26).header("origin","https://example.com").json(&equivalent).send().await.expect("UUID values retain input binding");
            assert_eq!(response.status(),201); assert_eq!(response.json::<serde_json::Value>().await.expect("original assignment"),assigned);
            let mut changed=input.clone(); changed["target"]["mobile"]=serde_json::json!("13800138001");
            for (operation,body,code,source) in [(operation,changed,"request_mismatch",27),(Uuid::new_v4(),input.clone(),"target_changed",28)] {
                cadence.tick().await;
                let response=request(reqwest::Method::POST,"/api/v1/assignments",Some(&manager),operation,source).header("origin","https://example.com").json(&body).send().await.expect("original and stale confirmation");
                assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("conflict"),serde_json::json!({"code":code}));
            }
            replace_role(&base,&manager,"13800138000",&default,6).await;
            cadence.tick().await;
            let response=request(reqwest::Method::DELETE,&format!("/api/v1/roles/{}",definitions[0]["id"].as_str().expect("role ID")),Some(&manager),Uuid::new_v4(),29).header("origin","https://example.com").json(&serde_json::json!({"changeId":definitions[0]["changeId"]})).send().await.expect("delete former role");
            assert_eq!(response.status(),200);
            assert_eq!(read(&format!("/api/v1/assignments?personId={}",assigned["assignment"]["subject"]["personId"].as_str().expect("subject person")),&manager,50).send().await.expect("person history").json::<serde_json::Value>().await.expect("history")["count"],3,"both replacements already permanent");
            cadence.tick().await;
            let response=request(reqwest::Method::POST,"/api/v1/assignments",Some(&manager),operation,30).header("origin","https://example.com").json(&input).send().await.expect("replay after replacement and deletion");
            assert_eq!(response.status(),201); assert_eq!(response.json::<serde_json::Value>().await.expect("immutable original"),assigned);
            let retained=history(observer.client()).await;
            assert_eq!(retained.len(),4);
            assert_eq!(retained.iter().find(|entry| entry["id"]==serde_json::json!(id)).expect("permanent deleted-role appointment"),assignment);
            let response=read("/api/v1/assignments",&manager,49).send().await.expect("history survives replacement and role deletion");
            assert_eq!(response.status(),200);
            assert_eq!(response.json::<serde_json::Value>().await.expect("historical role never replaced with current definition"),serde_json::json!({"code":"ok","count":4,"assignments":retained}));
            cadence.tick().await;
            let response=request(reqwest::Method::POST,"/api/v1/roles",Some(&manager),Uuid::new_v4(),50).header("origin","https://example.com").json(&serde_json::json!({"name":"Maintenance only","level":200,"permissions":["institution.roles"]})).send().await.expect("maintenance-only definition");
            assert_eq!(response.status(),201);
            let maintenance=response.json::<serde_json::Value>().await.expect("maintenance role")["role"].clone();
            replace_role(&base,&manager,"13800138000",&maintenance,7).await;
            assert_eq!(read("/api/v1/roles",&ordinary,51).send().await.expect("maintenance authority is active").status(),200);
            let response=read("/api/v1/assignments",&ordinary,52).send().await.expect("maintenance does not grant history authority");
            assert_eq!(response.status(),403);
            assert_eq!(response.json::<serde_json::Value>().await.expect("precise assignment permission"),serde_json::json!({"code":"permission_denied"}));
            let document=client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            for (path,operation_id,status) in [("/api/v1/assignments/candidates","assignmentCandidate","200"),("/api/v1/assignments","assignRole","201")] {
                let operation=&document["paths"][path]["post"];
                assert_eq!(operation["operationId"],operation_id);
                for header in ["Operation-Id","Session-Id","Origin"] {
                    assert!(operation["parameters"].as_array().expect("headers").iter().any(|parameter|parameter["name"]==header&&parameter["required"]==true));
                }
                assert!(operation["responses"][status]["headers"].get("Cache-Control").is_some());
                assert!(operation["responses"].get("422").is_some());
            }
            let target=&document["components"]["schemas"]["appointment.Target"];
            assert_eq!(target["required"].as_array().expect("complete target").len(),5);
            let resolve=|schema:&serde_json::Value| match schema["$ref"].as_str() { Some(reference)=>document["components"]["schemas"][reference.strip_prefix("#/components/schemas/").expect("schema reference")].clone(),None=>schema.clone() };
            for field in ["accountId","personId","revisionId","assignmentId"] {
                assert_eq!(resolve(&target["properties"][field])["format"],"uuid");
            }
            let available=&document["paths"]["/api/v1/assignments/roles"];
            assert_eq!(available["get"]["operationId"],"assignableRoles");
            assert!(!available["get"]["parameters"].as_array().expect("GET metadata").iter().any(|parameter|parameter["name"]=="Origin"));
            assert!(available["head"]["responses"]["200"].get("content").is_none());
            let history=&document["paths"]["/api/v1/assignments"];
            assert_eq!(history["get"]["operationId"],"listAssignments");
            assert_eq!(history["post"]["operationId"],"assignRole");
            assert!(history["head"]["responses"]["200"].get("content").is_none());
            let parameters=history["get"]["parameters"].as_array().expect("history parameters");
            for header in ["Operation-Id","Session-Id"] {
                assert!(parameters.iter().any(|parameter|parameter["name"]==header&&parameter["in"]=="header"&&parameter["required"]==true));
            }
            assert!(!parameters.iter().any(|parameter|parameter["name"]=="Origin"));
            assert!(history["get"]["description"].as_str().expect("owner contract link").contains("docs/access/roles.md#指派"));
            for name in ["personId","operationId"] {
                let parameter=parameters.iter().find(|parameter|parameter["name"]==name).expect("typed history filter");
                assert_eq!(parameter["in"],"query");
                assert_eq!(parameter["required"],false);
                assert_eq!(parameter["schema"]["format"],"uuid");
            }
            for method in ["get","head"] {
                assert_eq!(history[method]["responses"]["200"]["headers"]["Cache-Control"]["description"],"no-store");
                assert!(history[method]["responses"]["429"]["headers"].get("Retry-After").is_some());
            }
            let page=resolve(&history["get"]["responses"]["200"]["content"]["application/json"]["schema"]);
            assert_eq!(page["required"].as_array().expect("history envelope").len(),3);
            let item=resolve(&page["properties"]["assignments"]["items"]);
            assert_eq!(item["required"].as_array().expect("complete historical item").len(),7);
            let started=tokio::time::Instant::now();
            for path in ["/api/v1/assignments/candidates","/api/v1/assignments","/api/v1/roles","/api/v1/assignments/candidates"] {
                let response=request(reqwest::Method::POST,path,None,Uuid::new_v4(),99).header("origin","https://example.com").json(&serde_json::json!({})).send().await.expect("shared write source budget");
                assert_eq!(response.status(),401);
            }
            let response=request(reqwest::Method::POST,"/api/v1/assignments",None,Uuid::new_v4(),99).header("origin","https://example.com").json(&serde_json::json!({})).send().await.expect("source exhausted before authentication");
            assert!(started.elapsed()<Duration::from_millis(500),"burst observed before one source unit refills");
            assert_eq!(response.status(),429); assert!(response.headers().contains_key("retry-after"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("safe rate failure"),serde_json::json!({"code":"rate_limited"}));
            assert_eq!(read("/api/v1/assignments/roles",&manager,99).send().await.expect("independent metadata budget").status(),200);
            assert_eq!(read("/api/v1/assignments",&manager,99).send().await.expect("history also uses metadata instead of write budget").status(),200);
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application,result)=task.await.expect("HTTP ended"); result.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);
            assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no extra PNVS through replay and drain");
            sms.stop().await; drop(supplier);
        }).await.expect("bounded assignment HTTP flow");
    });
}

#[test]
fn workforce_assignment_history_exposes_complete_authorized_http_pages() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(45), async {
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
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.164.{address}"))
            };
            let path = "/api/v1/workforce/assignments";
            for actor in [&manager, &member] {
                let response = request(reqwest::Method::GET, path, actor, Uuid::new_v4()).send().await.expect("no domain authority");
                assert_eq!(response.status(), 403);
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe denial"), serde_json::json!({"code":"permission_denied"}));
            }
            let response = request(reqwest::Method::POST, "/api/v1/workforce/roles", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","name":"Assignment reader","level":200,"permissions":["workforce.assignments"]})).send().await.expect("reader role");
            assert_eq!(response.status(), 201);
            let role = response.json::<serde_json::Value>().await.expect("role")["role"].clone();
            let response = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("admission candidate");
            assert_eq!(response.status(), 200);
            let candidate = response.json::<serde_json::Value>().await.expect("confirmation");
            let full = candidate["roles"].as_array().expect("choices").iter().find(|role| role["kind"] == "full").expect("full role");
            let operation = Uuid::new_v4();
            let response = request(reqwest::Method::POST, "/api/v1/workforce/grants", &manager, operation)
                .json(&serde_json::json!({"entry":"institution","target":{
                    "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                    "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
                },"role":{"id":full["id"],"changeId":full["changeId"]}})).send().await.expect("real full membership");
            assert_eq!(response.status(), 201);
            let response = request(reqwest::Method::POST, "/api/v1/workforce/assignments/candidates", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("replacement candidate");
            assert_eq!(response.status(), 200);
            let candidate = response.json::<serde_json::Value>().await.expect("replacement confirmation");
            let body = serde_json::json!({"entry":"institution","target":{
                "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                "mobile":"13800138000","grantId":candidate["grantId"],"assignmentId":candidate["assignmentId"]
            },"role":{"id":role["id"],"changeId":role["changeId"]}});
            let response = request(reqwest::Method::POST, path, &manager, operation).json(&body).send().await.expect("independent replacement family");
            assert_eq!(response.status(), 201);
            let assigned = response.json::<serde_json::Value>().await.expect("replacement")["assignment"].clone();
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
            let facts = observer.client().query(r#"SELECT assignment.id,assignment.grant_id,assignment.sequence,assignment.assigned_at,assignment.operation_id,subject.person_id,subject.id AS revision_id,operator.person_id AS operator_person,operator.id AS operator_id,definition.role_id,definition.id AS change_id,definition.name,definition.level,ARRAY(SELECT capability_id FROM access.entries WHERE change_id=definition.id ORDER BY capability_id COLLATE "C") AS permissions
                FROM access.assignments assignment JOIN identity.revisions subject ON subject.id=assignment.revision_id JOIN identity.revisions operator ON operator.id=assignment.operator_id
                CROSS JOIN LATERAL (SELECT id,role_id,name,level FROM access.changes WHERE role_id=assignment.role_id AND changed_at<assignment.assigned_at ORDER BY changed_at DESC LIMIT 1) definition
                ORDER BY assignment.assigned_at DESC,assignment.id DESC"#, &[]).await.expect("independent fixed facts before HTTP reads");
            let expected: Vec<_> = facts.iter().map(|row| {
                let level: i16 = row.get("level");
                serde_json::json!({"id":row.get::<_,Uuid>("id"),"grantId":row.get::<_,Uuid>("grant_id"),"sequence":row.get::<_,i64>("sequence"),"assignedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("assigned_at"),"operationId":row.get::<_,Uuid>("operation_id"),
                    "subject":{"personId":row.get::<_,Uuid>("person_id"),"revisionId":row.get::<_,Uuid>("revision_id"),"name":"F*"},
                    "operator":{"personId":row.get::<_,Uuid>("operator_person"),"revisionId":row.get::<_,Uuid>("operator_id"),"name":"F*"},
                    "role":{"id":row.get::<_,Uuid>("role_id"),"changeId":row.get::<_,Uuid>("change_id"),"name":row.get::<_,String>("name"),"level":level,"kind":if level==1000 {"full"} else {"custom"},"permissions":row.get::<_,Vec<String>>("permissions")}})
            }).collect();
            assert_eq!(expected.len(), 2); assert_eq!(expected[0], assigned);
            let person = expected[0]["subject"]["personId"].as_str().expect("person ID");
            let read_id = Uuid::new_v4();
            for (query, count, assignments) in [
                (String::new(), 2, expected.clone()),
                ("?page=2&size=1".into(), 2, vec![expected[1].clone()]),
                ("?page=9223372036854775807&size=1".into(), 2, vec![]),
                (format!("?personId={person}"), 2, expected.clone()),
                (format!("?operationId={operation}"), 2, expected.clone()),
                (format!("?personId={person}&operationId={operation}"), 2, expected.clone()),
                (format!("?personId={}&operationId={operation}", Uuid::new_v4()), 0, vec![]),
                (format!("?operationId={}", Uuid::new_v4()), 0, vec![]),
            ] {
                let response = request(reqwest::Method::GET, &format!("{path}{query}"), &member, read_id).send().await.expect("history page");
                assert_eq!(response.status(), 200, "{query}");
                assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("complete eight-field assignments"), serde_json::json!({"code":"ok","count":count,"assignments":assignments}));
            }
            for query in ["page=0", "size=101", "page=1&page=2", "page=9223372036854775807&size=2", "entry=domain", "personId=", "personId=invalid", "operationId=", "operationId=invalid", "personId=00000000-0000-4000-a000-000000000001&personId=00000000-0000-4000-a000-000000000002"] {
                let response = request(reqwest::Method::GET, &format!("{path}?{query}"), &member, read_id).send().await.expect("invalid query");
                assert_eq!(response.status(), 400, "{query}");
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe input failure"), serde_json::json!({"code":"invalid_input"}));
            }
            let response = request(reqwest::Method::HEAD, path, &member, read_id).send().await.expect("HEAD history");
            assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie")); assert!(response.bytes().await.expect("HEAD bytes").is_empty());
            assert_eq!(request(reqwest::Method::HEAD, path, &manager, read_id).send().await.expect("HEAD denied").status(), 403);
            assert_eq!(request(reqwest::Method::GET, path, &member, read_id).header("sec-fetch-site", "cross-site").send().await.expect("cross-site denied").status(), 403);
            let response = client.get(format!("{base}{path}")).header("operation-id", read_id.to_string()).header("session-id", &member.0).header("cookie", &member.1)
                .header("sec-fetch-site", "same-origin").header("x-real-ip", "203.0.164.250").send().await.expect("same-origin metadata without Origin");
            assert_eq!(response.status(), 200);
            let response = client.get(format!("{base}{path}")).header("operation-id", read_id.to_string()).header("x-real-ip", "203.0.164.251").send().await.expect("missing identity");
            assert_eq!(response.status(), 401);
            let owner = connections(&server, &name, "postgres").recover().await.expect("isolated query failure");
            owner.client().batch_execute("REVOKE SELECT ON identity.revisions FROM clerk").await.expect("revoke only test permission");
            let response = request(reqwest::Method::GET, path, &member, read_id).send().await.expect("dependency failure");
            assert_eq!(response.status(), 503);
            assert_eq!(response.json::<serde_json::Value>().await.expect("safe response"), serde_json::json!({"code":"dependency_unavailable"}));
            owner.client().batch_execute("GRANT SELECT ON identity.revisions TO clerk").await.expect("restore permission");
            assert_eq!(request(reqwest::Method::GET, path, &member, read_id).send().await.expect("next complete read").status(), 200);
            let response = request(reqwest::Method::POST, path, &manager, operation).json(&body).send().await.expect("original write replay remains available");
            assert_eq!(response.status(), 201); assert_eq!(response.json::<serde_json::Value>().await.expect("same original")["assignment"], assigned);
            assert_eq!(request(reqwest::Method::GET, path, &member, Uuid::new_v4()).send().await.expect("history after replay").json::<serde_json::Value>().await.expect("history")["count"], 2, "reads and replay preserve rows");
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let route = &document["paths"][path]["get"];
            assert_eq!(route["operationId"], "listWorkforceAssignments");
            assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/assignment.List");
            assert!(route["parameters"].as_array().expect("parameters").iter().all(|parameter| parameter["name"] != "Origin" && parameter["name"] != "entry"));
            for (field, owner) in [("personId", "person.Id"), ("operationId", "operation.Id")] {
                let parameter = route["parameters"].as_array().expect("parameters").iter().find(|parameter| parameter["name"] == field).expect("filter parameter");
                assert_eq!(parameter["required"], false);
                assert_eq!(parameter["schema"]["$ref"], format!("#/components/schemas/{owner}"), "{parameter}");
                assert_eq!(document["components"]["schemas"][owner]["format"], "uuid");
            }
            assert!(document["paths"][path]["head"]["responses"]["200"].get("content").is_none());
            assert!(document["paths"][path].get("post").is_some());
            owner.terminate().await.expect("owner ended"); observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application, ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
            assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded assignment HTTP acceptance");
    });
}

#[test]
fn workforce_replacement_confirms_current_members_and_returns_permanent_http_results() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(45), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await; let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(
                connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),
                supplier.client(), message_at(sms.address(), sms.authority()),
            ).await.expect("verified startup");
            let (base, stop, task) = running(application).await;
            let institution = issued_session_for(&base, &mut sms, "13900000000").await;
            let domain = issued_session_for(&base, &mut sms, "13800138000").await;
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent runtime observer");
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = std::cell::Cell::new(1_u8);
            let request = |path: &str, actor: &(String,String), operation: Uuid| {
                let address = source.get(); source.set(address+1);
                client.post(format!("{base}{path}")).header("operation-id", operation.to_string()).header("session-id", &actor.0).header("cookie", &actor.1).header("x-real-ip", format!("203.0.149.{address}"))
            };
            let mut cadence = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_millis(120), Duration::from_millis(120));
            cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let path = "/api/v1/workforce/assignments/candidates";
            let search = serde_json::json!({"entry":"institution","mobile":"13800138000"});
            for (builder,status,code) in [
                (request(path,&institution,Uuid::new_v4()).json(&search),403,"permission_denied"),
                (request(path,&institution,Uuid::new_v4()).header("origin","https://wrong.invalid").json(&search),403,"permission_denied"),
                (request(path,&institution,Uuid::new_v4()).header("origin","https://example.com").header("sec-fetch-site","cross-site").json(&search),403,"permission_denied"),
                (request(path,&domain,Uuid::new_v4()).header("origin","https://example.com").json(&search),403,"permission_denied"),
                (request(path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&search),404,"target_unavailable"),
                (request(path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800138000"})),422,"invalid_input"),
                (request(path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"invalid","mobile":"13800138000"})),422,"invalid_input"),
                (request("/api/v1/workforce/assignments/candidates?size=101",&institution,Uuid::new_v4()).header("origin","https://example.com").json(&search),400,"invalid_input"),
            ] {
                cadence.tick().await;
                let response = builder.send().await.expect("actual input and authority boundary"); assert_eq!(response.status(),status);
                assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe rejection"),serde_json::json!({"code":code}));
            }
            let response = request("/api/v1/workforce/roles",&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","name":"Operators","level":200,"permissions":[]})).send().await.expect("custom role");
            assert_eq!(response.status(),201); let role = response.json::<serde_json::Value>().await.expect("role")["role"].clone();
            for (actor,entry,mobile,kind) in [(&institution,"institution","13800138000","full"),(&domain,"domain","13900000000","default")] {
                let response = request("/api/v1/workforce/grants/candidates",actor,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":entry,"mobile":mobile})).send().await.expect("admission candidate"); assert_eq!(response.status(),200);
                let candidate = response.json::<serde_json::Value>().await.expect("candidate");
                let selected = candidate["roles"].as_array().expect("roles").iter().find(|role|role["kind"]==kind).expect("selected system role");
                let body = serde_json::json!({"entry":entry,"target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":mobile,"assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]},"role":{"id":selected["id"],"changeId":selected["changeId"]}});
                assert_eq!(request("/api/v1/workforce/grants",actor,Uuid::new_v4()).header("origin","https://example.com").json(&body).send().await.expect("real admission").status(),201);
            }
            for (actor,entry,mobile,expected_count) in [(&domain,"domain","13900000000",2),(&institution,"institution","13800138000",3)] {
                let search = serde_json::json!({"entry":entry,"mobile":mobile});
                let response = request("/api/v1/workforce/assignments/candidates?page=1&size=1",actor,Uuid::new_v4()).header("origin","https://example.com").json(&search).send().await.expect("paged member confirmation"); assert_eq!(response.status(),200);
                let candidate = response.json::<serde_json::Value>().await.expect("candidate");
                assert_eq!(candidate["count"],expected_count); assert_eq!(candidate["roles"].as_array().expect("one role").len(),1);
                assert_eq!(candidate["identity"].as_object().expect("masked identity").len(),5); assert_eq!(candidate["identity"]["name"],"F*"); assert_eq!(candidate["identity"]["idNumber"].as_object().expect("number suffix").len(),1);
                let grant_id: Uuid = candidate["grantId"].as_str().expect("grant").parse().expect("UUID");
                let before = observer.client().query_one("SELECT assignment_id,role_id FROM access.grants WHERE termination IS NULL AND id=$1", &[&grant_id]).await.expect("independent current member");
                assert_eq!(candidate["assignmentId"],before.get::<_,Uuid>("assignment_id").to_string()); assert_eq!(candidate["role"]["id"],before.get::<_,Uuid>("role_id").to_string());
                let body = serde_json::json!({"entry":entry,"target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":mobile,"grantId":candidate["grantId"],"assignmentId":candidate["assignmentId"]},"role":{"id":role["id"],"changeId":role["changeId"]}});
                let operation = Uuid::new_v4();
                let mut invalid = body.clone(); invalid["target"]["grantId"] = serde_json::json!("invalid");
                for (builder,status,code) in [
                    (request("/api/v1/workforce/assignments",actor,operation).json(&body),403,"permission_denied"),
                    (request("/api/v1/workforce/assignments?extra=1",actor,operation).header("origin","https://example.com").json(&body),400,"invalid_input"),
                    (request("/api/v1/workforce/assignments",actor,operation).header("origin","https://example.com").json(&invalid),422,"invalid_input"),
                ] {
                    cadence.tick().await;
                    let response = builder.send().await.expect("write input boundary"); assert_eq!(response.status(),status); assert_eq!(response.json::<serde_json::Value>().await.expect("failure"),serde_json::json!({"code":code}));
                }
                let response = request("/api/v1/workforce/assignments",actor,operation).header("origin","https://example.com").json(&body).send().await.expect("replacement");
                assert_eq!(response.status(),201); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                let original = response.json::<serde_json::Value>().await.expect("assignment result");
                assert_eq!(original["code"],"ok"); assert_eq!(original["assignment"]["role"],role); assert_eq!(original["assignment"]["sequence"],2); assert_eq!(original["assignment"]["grantId"],candidate["grantId"]);
                let fact = observer.client().query_one("SELECT assignment.id,assignment.revision_id,assignment.operator_id,assignment.operation_id,assignment.assigned_at,member.assignment_id,person.sequence AS person_sequence FROM access.assignments assignment JOIN access.grants member ON member.id=assignment.grant_id AND member.termination IS NULL JOIN identity.persons person ON person.id=assignment.person_id WHERE assignment.operation_id=$1 AND assignment.entry IS NOT NULL", &[&operation]).await.expect("independent committed assignment before replay");
                assert_eq!(fact.get::<_,Uuid>("id"),fact.get::<_,Uuid>("assignment_id")); assert_eq!(fact.get::<_,i64>("person_sequence"),1);
                assert_eq!(original["assignment"]["id"],fact.get::<_,Uuid>("id").to_string()); assert_eq!(original["assignment"]["subject"]["revisionId"],fact.get::<_,Uuid>("revision_id").to_string()); assert_eq!(original["assignment"]["operator"]["revisionId"],fact.get::<_,Uuid>("operator_id").to_string()); assert_eq!(original["assignment"]["assignedAt"],serde_json::to_value(fact.get::<_,chrono::DateTime<chrono::Utc>>("assigned_at")).expect("time"));
                let response = request("/api/v1/workforce/assignments",actor,operation).header("origin","https://example.com").json(&body).send().await.expect("same original"); assert_eq!(response.status(),201); assert_eq!(response.json::<serde_json::Value>().await.expect("original"),original);
                let mut mismatch = body.clone(); mismatch["role"]["changeId"] = serde_json::json!(Uuid::new_v4());
                for (value,key,code) in [(&mismatch,operation,"request_mismatch"),(&body,Uuid::new_v4(),"target_changed")] {
                    cadence.tick().await;
                    let response = request("/api/v1/workforce/assignments",actor,key).header("origin","https://example.com").json(value).send().await.expect("original and confirmation rejection"); assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("code"),serde_json::json!({"code":code}));
                }
                let response = request("/api/v1/workforce/assignments/candidates?page=9&size=100",actor,Uuid::new_v4()).header("origin","https://example.com").json(&search).send().await.expect("out of range page"); assert_eq!(response.status(),200); let page = response.json::<serde_json::Value>().await.expect("empty page"); assert_eq!(page["count"],expected_count); assert_eq!(page["roles"],serde_json::json!([]));
                let mut same = body.clone(); same["target"]["assignmentId"] = original["assignment"]["id"].clone();
                let response = request("/api/v1/workforce/assignments",actor,Uuid::new_v4()).header("origin","https://example.com").json(&same).send().await.expect("unchanged role"); assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("code"),serde_json::json!({"code":"state_conflict"}));
            }
            assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.assignments)=4 AND (SELECT count(*) FROM access.grants)=2 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=2 AND NOT EXISTS(SELECT 1 FROM identity.persons WHERE sequence<>1)", &[]).await.expect("only two replacements were written").get::<_,bool>(0));
            let schema = client.get(format!("{base}/openapi/v1.json")).send().await.expect("schema").json::<serde_json::Value>().await.expect("OpenAPI");
            for path in ["/api/v1/workforce/assignments/candidates","/api/v1/workforce/assignments"] {
                let params = schema["paths"][path]["post"]["parameters"].as_array().expect("headers");
                for name in ["Operation-Id","Session-Id","Origin"] { assert_eq!(params.iter().find(|value|value["name"]==name).expect("header")["required"],true); }
            }
            for (field,owner) in [("accountId","account.Id"),("personId","person.Id"),("revisionId","person.revision.Id"),("grantId","grant.Id"),("assignmentId","assignment.Id")] {
                assert_eq!(schema["components"]["schemas"]["assignment.Target"]["properties"][field]["$ref"],format!("#/components/schemas/{owner}"));
            }
            stop.send(()).expect("stop"); let (application,ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2); assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"supplier monitoring continues through replay and drain");
            observer.terminate().await.expect("observer ended"); sms.stop().await;
        }).await.expect("HTTP replacement deadline");
    });
}
