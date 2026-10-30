use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, role::*, supplier::*};

#[test]
fn account_suspension_confirms_private_targets_and_preserves_original_http_results() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(45), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server,&name,"clerk"),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("verified startup");
            let (base,stop,task) = running(application).await;
            let manager = issued_session_for(&base,&mut sms,"13900000000").await;
            let ordinary = issued_session_for(&base,&mut sms,"13800000000").await;
            let observer = connections(&server,&name,"clerk").recover().await.expect("independent runtime observer");
            let target = observer.client().query_one("SELECT account.id, account.person_id, person.revision_id, holder.assignment_id, person.sequence AS person_sequence FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id JOIN access.holders holder ON holder.id=account.id WHERE session.id=$1::text::uuid",&[&ordinary.0]).await.expect("real R2 target");
            let administrator = current_account(&base,&manager).await;
            let account_id:Uuid = target.get("id");
            let path = format!("/api/v1/accounts/{account_id}/suspensions");
            let input = serde_json::json!({"personId":target.get::<_,Uuid>("person_id"),"revisionId":target.get::<_,Uuid>("revision_id"),"mobile":"13800000000"});
            let candidate = account_candidate(&base,&manager,"13800000000").await;
            assert_eq!(candidate,serde_json::json!({"code":"ok","identity":{"accountId":account_id,"personId":target.get::<_,Uuid>("person_id"),"revisionId":target.get::<_,Uuid>("revision_id"),"name":"F*","idNumber":{"suffix":"007X"}},"suspensionId":null}));
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client without implicit Origin");
            let request = |path:&str,identity:Option<&(String,String)>,operation:Uuid,source:u8| {
                let mut request=client.post(format!("{base}{path}")).header("operation-id",operation.to_string()).header("x-real-ip",format!("203.0.118.{source}"));
                if let Some(identity)=identity {request=request.header("session-id",&identity.0).header("cookie",&identity.1);}
                request
            };
            let operation=Uuid::new_v4();
            let mut invalid=input.clone();invalid["revisionId"]=serde_json::json!("private-invalid-revision");
            let mut unknown=input.clone();unknown["extra"]=serde_json::json!(true);
            let mut changed=input.clone();changed["personId"]=serde_json::json!(Uuid::new_v4());
            let mut cadence=tokio::time::interval_at(tokio::time::Instant::now()+Duration::from_millis(120),Duration::from_millis(120));
            for (builder,status,code) in [
                (request("/api/v1/accounts/not-a-uuid/suspensions",None,operation,2).json(&invalid),400,"invalid_input"),
                (request(&path,None,operation,3).header("origin","https://example.com").json(&invalid),401,"authentication_invalid"),
                (request(&path,Some(&manager),operation,4).header("sec-fetch-site","same-origin").json(&invalid),403,"permission_denied"),
                (request(&path,Some(&ordinary),operation,5).header("origin","https://example.com").json(&invalid),422,"invalid_input"),
                (request(&path,Some(&ordinary),operation,6).header("origin","https://example.com").json(&input),403,"permission_denied"),
                (request(&path,Some(&manager),operation,7).header("origin","https://example.com").json(&unknown),422,"invalid_input"),
                (request(&path,Some(&manager),operation,8).header("origin","https://example.com").json(&changed),409,"target_changed"),
                (request(&path,Some(&manager),operation,9).header("origin","https://wrong.invalid").json(&input),403,"permission_denied"),
                (request(&path,Some(&manager),operation,10).header("origin","https://example.com").body("{}"),415,"invalid_input"),
                (request(&path,Some(&manager),operation,11).header("origin","https://example.com").header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
                (request("/api/v1/accounts/candidates",Some(&ordinary),operation,12).header("origin","https://example.com").json(&serde_json::json!({"mobile":"138"})),422,"invalid_input"),
                (request("/api/v1/accounts/candidates",Some(&ordinary),operation,13).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13700000000"})),403,"permission_denied"),
                (request("/api/v1/accounts/candidates",Some(&manager),operation,14).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13900000000"})),404,"target_unavailable"),
                (request("/api/v1/accounts/candidates",Some(&manager),operation,15).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13700000000"})),404,"target_unavailable"),
                (request("/api/v1/accounts/candidates",Some(&manager),operation,16).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800000000","extra":1})),422,"invalid_input"),
            ] {
                cadence.tick().await;
                let response=builder.send().await.expect("ordered rejection");
                assert_eq!(response.status(),status);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe error envelope"),serde_json::json!({"code":code}));
            }
            let mut roles=Vec::new();
            for (index,permissions) in [serde_json::json!(["institution.roles"]),serde_json::json!(["institution.accounts"])].into_iter().enumerate() {
                cadence.tick().await;
                let response=request("/api/v1/roles",Some(&manager),Uuid::new_v4(),u8::try_from(20+index).expect("source")).header("origin","https://example.com")
                    .json(&serde_json::json!({"name":format!("Suspension qualification {index}"),"level":200,"permissions":permissions})).send().await.expect("real narrow role");
                assert_eq!(response.status(),201);roles.push(response.json::<serde_json::Value>().await.expect("role")["role"].clone());
            }
            replace_role(&base,&manager,"13800000000",&roles[0],81).await;
            cadence.tick().await;
            let response=request("/api/v1/accounts/candidates",Some(&ordinary),Uuid::new_v4(),22).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13900000000"})).send().await.expect("maintenance alone grants no account permission");
            assert_eq!(response.status(),403);assert_eq!(response.json::<serde_json::Value>().await.expect("denied"),serde_json::json!({"code":"permission_denied"}));
            replace_role(&base,&manager,"13800000000",&roles[1],82).await;
            for mobile in ["13900000000","13800000000"] {
                cadence.tick().await;
                let response=request("/api/v1/accounts/candidates",Some(&ordinary),Uuid::new_v4(),23).header("origin","https://example.com").json(&serde_json::json!({"mobile":mobile})).send().await.expect("account permission still hides superior and self");
                assert_eq!(response.status(),404);assert_eq!(response.json::<serde_json::Value>().await.expect("hidden target"),serde_json::json!({"code":"target_unavailable"}));
            }
            let before=observer.client().query_one("SELECT person.sequence AS person_sequence,account.sequence AS account_sequence,account.version AS account_version,holder.assignment_id,(SELECT count(*) FROM identity.suspensions) AS suspensions FROM identity.persons person JOIN identity.accounts account ON account.person_id=person.id JOIN access.holders holder ON holder.person_id=person.id WHERE holder.id=$1",&[&account_id]).await.expect("pre-suspension state");
            assert_eq!(before.get::<_,i64>("suspensions"),0);
            assert_eq!((before.get::<_,i64>("account_sequence"),before.get::<_,i64>("account_version"),before.get::<_,i64>("person_sequence")),(1,1,1));
            cadence.tick().await;
            let response=request(&path,Some(&manager),operation,24).header("origin","https://example.com").json(&input).send().await.expect("actual suspension");
            assert_eq!(response.status(),201);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
            let original=response.json::<serde_json::Value>().await.expect("permanent result");
            let row=observer.client().query_one("SELECT suspension.id, suspension.suspended_at, suspension.account_id=$2 AND suspension.person_id=$3 AND suspension.revision_id=$4 AND suspension.operator_id=$5 AS bound, suspension.sequence=account.sequence AND account.sequence=$6::bigint+1 AND account.version=2 AND account.changed_at=suspension.suspended_at AND account.suspension_id=suspension.id AND person.sequence=$9 AS advanced, holder.assignment_id=$7 AS unchanged_assignment, revocation.id=$8::text::uuid AND revocation.mode='suspension' AND revocation.cause_id=suspension.id AND revocation.operation_id=suspension.operation_id AND revocation.revoked_at=suspension.suspended_at AND revocation.transaction_id=suspension.transaction_id AND revocation.sequence=2 AND session.sequence=2 AND session.version=2 AND session.revoked_at=revocation.revoked_at AND suspension.sessions=ARRAY[session.id] AS atomic, NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=revocation.id) AS no_handoff FROM identity.suspensions suspension JOIN identity.persons person ON person.id=suspension.person_id JOIN identity.accounts account ON account.id=suspension.account_id JOIN access.holders holder ON holder.id=suspension.account_id JOIN identity.revocations revocation ON revocation.cause_id=suspension.id AND revocation.mode='suspension' JOIN identity.sessions session ON session.id=revocation.id AND session.account_id=account.id WHERE suspension.operation_id=$1",&[&operation,&account_id,&target.get::<_,Uuid>("person_id"),&target.get::<_,Uuid>("revision_id"),&uuid_of(&administrator["revisionId"]),&before.get::<_,i64>("account_sequence"),&before.get::<_,Uuid>("assignment_id"),&ordinary.0,&before.get::<_,i64>("person_sequence")]).await.expect("independent complete facts before replay");
            for field in ["bound","advanced","unchanged_assignment","atomic","no_handoff"] {assert!(row.get::<_,bool>(field),"{field}");}
            let expected=serde_json::json!({"code":"ok","suspension":{"id":row.get::<_,Uuid>("id"),"suspendedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("suspended_at")}});
            assert_eq!(original,expected);
            assert_eq!(account_candidate(&base,&manager,"13800000000").await["suspensionId"],expected["suspension"]["id"]);
            for (identity,status) in [(&ordinary,401),(&manager,200)] {
                let response=http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&identity.0).header("cookie",&identity.1).send().await.expect("session consequences");
                assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie"));
            }
            cadence.tick().await;
            let response=request(&path,Some(&manager),operation,25).header("origin","https://example.com").json(&input).send().await.expect("same operation replay precedes current paused state");
            assert_eq!(response.status(),201);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("original result"),expected);
            for (index,field,value) in [(0,"personId",serde_json::json!(Uuid::new_v4())),(1,"revisionId",serde_json::json!(Uuid::new_v4())),(2,"mobile",serde_json::json!("13700000000"))] {
                cadence.tick().await;
                let mut different=input.clone();different[field]=value;
                let response=request(&path,Some(&manager),operation,u8::try_from(26+index).expect("source")).header("origin","https://example.com").json(&different).send().await.expect("permanent original input binding");
                assert_eq!(response.status(),409);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch"),serde_json::json!({"code":"request_mismatch"}));
            }
            cadence.tick().await;
            let response=request(&format!("/api/v1/accounts/{}/suspensions",uuid_of(&administrator["id"])),Some(&manager),operation,29).header("origin","https://example.com").json(&input).send().await.expect("account binding precedes new-target hiding");
            assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch"),serde_json::json!({"code":"request_mismatch"}));
            cadence.tick().await;
            let response=request(&path,Some(&manager),Uuid::new_v4(),30).header("origin","https://example.com").json(&input).send().await.expect("new operation cannot pause twice");
            assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("state conflict"),serde_json::json!({"code":"state_conflict"}));
            let document=client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            for endpoint in ["/api/v1/accounts/candidates",path.as_str()] {
                let response=client.put(format!("{base}{endpoint}")).header("operation-id",Uuid::new_v4().to_string()).send().await.expect("unsupported method keeps the common envelope");
                assert_eq!(response.status(),405);
                let mut allowed:Vec<_>=response.headers()["allow"].to_str().expect("allowed methods").split(',').map(str::trim).collect();allowed.sort_unstable();
                assert_eq!(allowed,if endpoint=="/api/v1/accounts/candidates" {vec!["POST"]}else{vec!["GET","HEAD","POST"]});assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("method error"),serde_json::json!({"code":"method_not_allowed"}));
            }
            for (path,id,status) in [("/api/v1/accounts/candidates","accountCandidate","200"),("/api/v1/accounts/{id}/suspensions","suspendAccount","201")] {
                let operation=&document["paths"][path]["post"];assert_eq!(operation["operationId"],id);
                for header in ["Operation-Id","Session-Id","Origin"] {assert!(operation["parameters"].as_array().expect("parameters").iter().any(|parameter|parameter["name"]==header&&parameter["required"]==true));}
                assert!(operation["responses"][status]["headers"].get("Cache-Control").is_some());
                assert!(operation["responses"].get("422").is_some());
                assert_eq!(document["paths"][path].get("get").is_some(),path=="/api/v1/accounts/{id}/suspensions");
            }
            let schema=&document["components"]["schemas"]["suspension.Input"];
            assert_eq!(schema["required"],serde_json::json!(["personId","revisionId","mobile"]));
            assert_eq!(schema["additionalProperties"],false);
            let candidate=&document["components"]["schemas"]["account.Candidate"];
            assert!(candidate["required"].as_array().expect("required candidate fields").contains(&serde_json::json!("suspensionId")));
            assert!(candidate["properties"]["suspensionId"]["oneOf"].as_array().expect("nullable original suspension").iter().any(|value|value["type"]=="null"));
            let receipt=&document["components"]["schemas"]["suspension.Suspension"];
            assert_eq!(receipt["properties"].as_object().expect("minimal permanent result").len(),2);
            // Both POSTs debit the same source bucket before authentication; GET keeps its own budget.
            cadence.tick().await;
            let started=tokio::time::Instant::now();
            for index in 0..5 {
                let endpoint=if index%2==0 {"/api/v1/accounts/candidates"} else {&path};
                let response=request(endpoint,None,Uuid::new_v4(),99).header("origin","https://example.com").json(&input).send().await.expect("shared write budget");
                assert_eq!(response.status(),if index<4 {401}else{429});
                if index==4 {assert_eq!(response.headers()["retry-after"],"1");assert_eq!(response.json::<serde_json::Value>().await.expect("rate limit"),serde_json::json!({"code":"rate_limited"}));}
            }
            assert!(started.elapsed()<Duration::from_millis(500),"source bucket observation precedes refill");
            assert_eq!(http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&manager.0).header("cookie",&manager.1).header("x-real-ip","203.0.118.99").send().await.expect("independent metadata budget").status(),200);
            let counts=observer.client().query_one("SELECT (SELECT count(*) FROM identity.suspensions WHERE operation_id=$1) AS suspensions,(SELECT count(*) FROM identity.revocations WHERE mode='suspension' AND cause_id=$2) AS revocations,(SELECT sequence AS person_sequence FROM identity.persons WHERE id=$3) AS sequence,(SELECT sequence FROM identity.accounts WHERE id=$4) AS account_sequence",&[&operation,&row.get::<_,Uuid>("id"),&target.get::<_,Uuid>("person_id"),&account_id]).await.expect("replays do not append or broaden original work");
            assert_eq!(counts.get::<_,i64>("suspensions"),1);assert_eq!(counts.get::<_,i64>("revocations"),1);assert_eq!(counts.get::<_,i64>("sequence"),before.get::<_,i64>("person_sequence"));assert_eq!(counts.get::<_,i64>("account_sequence"),2);
            stop.send(()).expect("stop");let (application,result)=task.await.expect("server");result.expect("drained");drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no extra supplier request through replay and drain");
            observer.terminate().await.expect("observer ended");sms.stop().await;
        }).await.expect("suspension protocol budget");
    });
}

#[test]
fn account_resumption_preserves_original_http_results_across_later_suspension() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(60), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server,&name,"clerk"),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("verified startup");
            let issuance = application.registration().clone();
            let (base,stop,task) = running(application).await;
            let administrator = issued_session_for(&base,&mut sms,"13900000000").await;
            let target = issued_session_for(&base,&mut sms,"13800000000").await;
            let manager = issued_session_with_identity(&base,&mut sms,"13700000000","198.51.100.92","000000200002290029").await;
            // Move the complete issuance graph before either permanent account-state fact.
            age_issuance(&server,&name, &issuance).await;
            let observer = connections(&server,&name,"clerk").recover().await.expect("independent runtime observer");
            let subject = observer.client().query_one("SELECT account.id,account.person_id,person.revision_id,holder.assignment_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id JOIN access.holders holder ON holder.id=account.id WHERE session.id=$1::text::uuid",&[&target.0]).await.expect("independent R2 target identity");
            let operator = current_account(&base,&manager).await;
            let account_id:Uuid = subject.get("id");
            let history_path=format!("/api/v1/accounts/{account_id}/suspensions");
            let empty=http_client().get(format!("{base}{history_path}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&administrator.0).header("cookie",&administrator.1).header("x-real-ip","203.0.122.1").send().await.expect("empty account history");
            assert_eq!(empty.status(),200);assert_eq!(empty.headers()["cache-control"],"no-store");assert!(!empty.headers().contains_key("set-cookie"));assert_eq!(empty.json::<serde_json::Value>().await.expect("empty page"),serde_json::json!({"code":"ok","count":0,"suspensions":[]}));
            let candidate = account_candidate(&base,&administrator,"13800000000").await;
            let paused = suspend_account(&base,&administrator,&candidate,"13800000000").await;
            let paused_candidate = account_candidate(&base,&administrator,"13800000000").await;
            assert_eq!(paused_candidate["identity"],candidate["identity"]);
            assert_eq!(paused_candidate["suspensionId"],paused["suspension"]["id"]);
            let suspension_id:Uuid = serde_json::from_value(paused["suspension"]["id"].clone()).expect("suspension UUID");
            let path = format!("/api/v1/accounts/{account_id}/resumptions");
            let input = serde_json::json!({"personId":subject.get::<_,Uuid>("person_id"),"revisionId":subject.get::<_,Uuid>("revision_id"),"mobile":"13800000000","suspensionId":suspension_id});
            let before = observer.client().query_one("SELECT (SELECT sequence AS person_sequence FROM identity.persons WHERE id=$1) AS sequence,(SELECT sequence FROM identity.accounts WHERE id=$2) AS account_sequence,(SELECT count(*) FROM identity.sessions WHERE account_id=$2) AS sessions,(SELECT count(*) FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=$2) AS handoffs,(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM identity.revocations revocation WHERE account_id=$2)::text AS revocations",&[&subject.get::<_,Uuid>("person_id"),&account_id]).await.expect("original effects before resumption");
            assert_eq!(before.get::<_,i64>("sequence"),1);
            assert_eq!(before.get::<_,i64>("account_sequence"),2);
            assert_eq!(before.get::<_,i64>("sessions"),1);
            assert_eq!(before.get::<_,i64>("handoffs"),0);
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client without implicit Origin");
            let request = |path:&str,identity:Option<&(String,String)>,operation:Uuid,source:u8| {
                let mut request=client.post(format!("{base}{path}")).header("operation-id",operation.to_string()).header("x-real-ip",format!("203.0.121.{source}"));
                if let Some(identity)=identity {request=request.header("session-id",&identity.0).header("cookie",&identity.1);}
                request
            };
            let operation = Uuid::new_v4();
            let mut invalid=input.clone();invalid["suspensionId"]=serde_json::json!("private-invalid-suspension");
            let mut unknown=input.clone();unknown["extra"]=serde_json::json!(true);
            let mut missing=input.clone();missing.as_object_mut().expect("input").remove("suspensionId");
            let mut null=input.clone();null["suspensionId"]=serde_json::Value::Null;
            let mut invalid_mobile=input.clone();invalid_mobile["mobile"]=serde_json::json!("138");
            let mut changed=input.clone();changed["revisionId"]=serde_json::json!(Uuid::new_v4());
            let mut cadence=tokio::time::interval_at(tokio::time::Instant::now()+Duration::from_millis(120),Duration::from_millis(120));
            for (builder,status,code) in [
                (request("/api/v1/accounts/not-a-uuid/resumptions",None,operation,2).json(&invalid),400,"invalid_input"),
                (client.post(format!("{base}{path}")).header("origin","https://example.com").json(&input),400,"invalid_input"),
                (request(&path,None,operation,3).header("origin","https://example.com").json(&invalid),401,"authentication_invalid"),
                (request(&path,None,operation,4).header("session-id",&administrator.0).header("origin","https://example.com").json(&input),401,"authentication_invalid"),
                (request(&path,None,operation,5).header("cookie",&administrator.1).header("origin","https://example.com").json(&input),401,"authentication_invalid"),
                (request(&path,Some(&administrator),operation,6).header("sec-fetch-site","same-origin").json(&invalid),403,"permission_denied"),
                (request(&path,Some(&administrator),operation,7).header("origin","https://wrong.invalid").json(&input),403,"permission_denied"),
                (request(&path,Some(&manager),operation,8).header("origin","https://example.com").json(&invalid),422,"invalid_input"),
                (request(&path,Some(&manager),operation,9).header("origin","https://example.com").json(&input),403,"permission_denied"),
                (request(&path,Some(&administrator),operation,10).header("origin","https://example.com").json(&unknown),422,"invalid_input"),
                (request(&path,Some(&administrator),operation,11).header("origin","https://example.com").json(&missing),422,"invalid_input"),
                (request(&path,Some(&administrator),operation,12).header("origin","https://example.com").json(&null),422,"invalid_input"),
                (request(&path,Some(&administrator),operation,13).header("origin","https://example.com").json(&invalid_mobile),422,"invalid_input"),
                (request(&path,Some(&administrator),operation,14).header("origin","https://example.com").json(&changed),409,"target_changed"),
                (request(&path,Some(&administrator),operation,15).header("origin","https://example.com").body("{}"),415,"invalid_input"),
                (request(&path,Some(&administrator),operation,16).header("origin","https://example.com").header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
            ] {
                cadence.tick().await;
                let response=builder.send().await.expect("ordered resumption rejection");
                assert_eq!(response.status(),status);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe error envelope"),serde_json::json!({"code":code}));
            }
            for (index,permission) in ["institution.roles","institution.accounts"].into_iter().enumerate() {
                cadence.tick().await;
                let response=request("/api/v1/roles",Some(&administrator),Uuid::new_v4(),20).header("origin","https://example.com").json(&serde_json::json!({"name":format!("Resumption authority {index}"),"level":200,"permissions":[permission]})).send().await.expect("real narrow role");
                assert_eq!(response.status(),201);
                let role=response.json::<serde_json::Value>().await.expect("role")["role"].clone();
                replace_role(&base,&administrator,"13700000000",&role,u8::try_from(83+index).expect("source")).await;
                if index==0 {
                    cadence.tick().await;
                    let response=request(&path,Some(&manager),operation,21).header("origin","https://example.com").json(&input).send().await.expect("role maintenance is not account management");
                    assert_eq!(response.status(),403);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("denied"),serde_json::json!({"code":"permission_denied"}));
                    let response=http_client().get(format!("{base}{history_path}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&manager.0).header("cookie",&manager.1).header("x-real-ip","203.0.122.2").send().await.expect("maintenance cannot read account history");
                    assert_eq!(response.status(),403);assert_eq!(response.json::<serde_json::Value>().await.expect("exact history permission"),serde_json::json!({"code":"permission_denied"}));
                }
            }
            assert_eq!(account_candidate(&base,&manager,"13800000000").await,paused_candidate,"the exact account permission can confirm a lower paused target");
            let admin_account=uuid_of(&current_account(&base,&administrator).await["id"]);
            for target_id in [uuid_of(&operator["id"]),admin_account] {
                cadence.tick().await;
                let response=request(&format!("/api/v1/accounts/{target_id}/resumptions"),Some(&manager),Uuid::new_v4(),22).header("origin","https://example.com").json(&input).send().await.expect("self and superior stay hidden before confirmation");
                assert_eq!(response.status(),404);assert_eq!(response.json::<serde_json::Value>().await.expect("target unavailable"),serde_json::json!({"code":"target_unavailable"}));
            }
            cadence.tick().await;
            let response=request(&path,Some(&manager),operation,23).header("origin","https://example.com").json(&input).send().await.expect("account permission alone resumes the target");
            assert_eq!(response.status(),200);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
            let original=response.json::<serde_json::Value>().await.expect("original resumption");
            let row=observer.client().query_one("SELECT resumption.id,resumption.resumed_at,resumption.person_id=$3 AND resumption.revision_id=$4 AND resumption.operator_id=$5 AND suspension.account_id=$2 AND resumption.id=$6 AS bound,resumption.sequence=3 AND account.sequence=resumption.sequence AND account.version=3 AND account.suspension_id IS NULL AND account.changed_at=resumption.resumed_at AND person.sequence=1 AND resumption.resumed_at>=suspension.suspended_at AS advanced,octet_length(resumption.mobile)=48 AS sealed,holder.assignment_id=$7 AS unchanged_assignment,(SELECT count(*) FROM identity.sessions WHERE account_id=$2) AS sessions,(SELECT count(*) FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=$2) AS handoffs,(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM identity.revocations revocation WHERE account_id=$2)::text AS revocations FROM identity.resumptions resumption JOIN identity.suspensions suspension ON suspension.id=resumption.id JOIN identity.persons person ON person.id=resumption.person_id JOIN identity.accounts account ON account.id=resumption.account_id JOIN access.holders holder ON holder.id=suspension.account_id WHERE resumption.operation_id=$1",&[&operation,&account_id,&subject.get::<_,Uuid>("person_id"),&subject.get::<_,Uuid>("revision_id"),&uuid_of(&operator["revisionId"]),&suspension_id,&subject.get::<_,Uuid>("assignment_id")]).await.expect("independent permanent resumption before replay");
            for field in ["bound","advanced","sealed","unchanged_assignment"] {assert!(row.get::<_,bool>(field),"{field}");}
            assert_eq!(row.get::<_,i64>("sessions"),before.get::<_,i64>("sessions"));assert_eq!(row.get::<_,i64>("handoffs"),0);
            assert_eq!(row.get::<_,String>("revocations"),before.get::<_,String>("revocations"));
            let expected=serde_json::json!({"code":"ok","resumption":{"id":row.get::<_,Uuid>("id"),"resumedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("resumed_at")}});
            assert_eq!(original,expected);
            assert_eq!(account_candidate(&base,&manager,"13800000000").await["suspensionId"],serde_json::Value::Null);
            let renewed=issued_session_from(&base,&mut sms,"13800000000","198.51.100.93").await;
            assert_ne!(renewed.0,target.0,"resumption never revives the old session");
            for (identity,status) in [(&target,401),(&renewed,200),(&manager,200),(&administrator,200)] {
                let response=http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&identity.0).header("cookie",&identity.1).send().await.expect("old and new credential eligibility");
                assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie"));
            }
            let later=suspend_account(&base,&manager,&candidate,"13800000000").await;
            let later_id:Uuid=serde_json::from_value(later["suspension"]["id"].clone()).expect("later suspension UUID");assert_ne!(later_id,suspension_id);
            let facts=observer.client().query_one("SELECT person.sequence=1 AND account.sequence=4 AND account.version=4 AND account.suspension_id=$3 AND holder.assignment_id=$2 AS stable,EXISTS(SELECT 1 FROM identity.suspensions WHERE id=$3 AND sequence=4 AND NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE id=$3)) AS open,EXISTS(SELECT 1 FROM identity.revocations WHERE id=$4::text::uuid AND mode='suspension' AND cause_id=$3 AND sequence=2 AND EXISTS(SELECT 1 FROM identity.sessions WHERE id=$4::text::uuid AND sequence=2 AND version=2)) AS renewed_revoked,NOT EXISTS(SELECT 1 FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=$1) AS no_handoffs FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id JOIN access.holders holder ON holder.id=account.id WHERE account.id=$1",&[&account_id,&subject.get::<_,Uuid>("assignment_id"),&later_id,&renewed.0]).await.expect("later committed state before any old replay");
            for field in ["stable","open","renewed_revoked","no_handoffs"] {assert!(facts.get::<_,bool>(field),"{field}");}
            cadence.tick().await;
            let response=request(&path,Some(&manager),operation,24).header("origin","https://example.com").json(&input).send().await.expect("old resumption returns its original completion");
            assert_eq!(response.status(),200);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("same original"),expected);
            for (field,value) in [("personId",serde_json::json!(Uuid::new_v4())),("revisionId",serde_json::json!(Uuid::new_v4())),("mobile",serde_json::json!("13600000000")),("suspensionId",serde_json::json!(later_id))] {
                cadence.tick().await;
                let mut different=input.clone();different[field]=value;
                let response=request(&path,Some(&manager),operation,25).header("origin","https://example.com").json(&different).send().await.expect("original input binding");
                assert_eq!(response.status(),409);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch"),serde_json::json!({"code":"request_mismatch"}));
            }
            for (identity,target_id) in [(&manager,admin_account),(&administrator,account_id)] {
                cadence.tick().await;
                let response=request(&format!("/api/v1/accounts/{target_id}/resumptions"),Some(identity),operation,27).header("origin","https://example.com").json(&input).send().await.expect("original account and operator binding");
                assert_eq!(response.status(),409);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch"),serde_json::json!({"code":"request_mismatch"}));
            }
            cadence.tick().await;
            let response=request(&path,Some(&manager),Uuid::new_v4(),26).header("origin","https://example.com").json(&input).send().await.expect("new operation cannot resume the old suspension");
            assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("state conflict"),serde_json::json!({"code":"state_conflict"}));
            assert_eq!(account_candidate(&base,&manager,"13800000000").await["suspensionId"],serde_json::json!(later_id));
            assert!(observer.client().query_one("SELECT (SELECT count(*) FROM identity.resumptions WHERE person_id=$1)=1 AND (SELECT sequence AS person_sequence FROM identity.persons WHERE id=$1)=1 AND (SELECT sequence FROM identity.accounts WHERE id=$2)=4 AND (SELECT count(*) FROM identity.sessions WHERE account_id=$2)=2 AND (SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM identity.revocations revocation WHERE mode='suspension' AND cause_id=$3)=$4::text::jsonb",&[&subject.get::<_,Uuid>("person_id"),&account_id,&suspension_id,&before.get::<_,String>("revocations")]).await.expect("old replay neither resumes B nor rewrites A").get::<_,bool>(0));
            for identity in [&target,&renewed] {
                let response=http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&identity.0).header("cookie",&identity.1).send().await.expect("both old sessions remain unusable");assert_eq!(response.status(),401);assert!(!response.headers().contains_key("set-cookie"));
            }
            let document=client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let described=&document["paths"]["/api/v1/accounts/{id}/resumptions"]["post"];
            assert_eq!(described["operationId"],"resumeAccount");
            for header in ["Operation-Id","Session-Id","Origin"] {assert!(described["parameters"].as_array().expect("parameters").iter().any(|parameter|parameter["name"]==header&&parameter["required"]==true));}
            for status in ["200","400","401","403","404","409","413","415","422","429","503"] {assert!(described["responses"][status]["headers"].get("Cache-Control").is_some(),"{status}");}
            assert!(described["responses"]["429"]["headers"].get("Retry-After").is_some());
            let resolve=|schema:&serde_json::Value| {document.pointer(schema["$ref"].as_str().expect("schema reference").strip_prefix('#').expect("local reference")).expect("component")};
            let schema=resolve(&described["requestBody"]["content"]["application/json"]["schema"]);
            assert_eq!(schema["required"],serde_json::json!(["personId","revisionId","mobile","suspensionId"]));assert_eq!(schema["additionalProperties"],false);assert_eq!(schema["properties"].as_object().expect("flat input").len(),4);
            let receipt=resolve(&described["responses"]["200"]["content"]["application/json"]["schema"]);
            assert_eq!(receipt["properties"].as_object().expect("result envelope").len(),2);
            let result=resolve(&receipt["properties"]["resumption"]);assert_eq!(result["required"],serde_json::json!(["id","resumedAt"]));assert_eq!(result["properties"].as_object().expect("minimal result").len(),2);
            let response=client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).send().await.expect("unsupported method");assert_eq!(response.status(),405);assert_eq!(response.headers()["allow"],"POST");assert_eq!(response.json::<serde_json::Value>().await.expect("method envelope"),serde_json::json!({"code":"method_not_allowed"}));
            // All three account POSTs share admission, while metadata remains independent.
            cadence.tick().await;
            let started=tokio::time::Instant::now();
            for (index,endpoint) in [path.clone(),"/api/v1/accounts/candidates".into(),format!("/api/v1/accounts/{account_id}/suspensions"),path.clone(),path.clone()].into_iter().enumerate() {
                let response=request(&endpoint,None,Uuid::new_v4(),99).header("origin","https://example.com").json(&input).send().await.expect("shared account write admission");
                assert_eq!(response.status(),if index<4 {401}else{429});
                if index==4 {assert_eq!(response.headers()["retry-after"],"1");assert_eq!(response.json::<serde_json::Value>().await.expect("rate limit"),serde_json::json!({"code":"rate_limited"}));}
            }
            assert!(started.elapsed()<Duration::from_millis(500),"observe before source refill");
            assert_eq!(http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&manager.0).header("cookie",&manager.1).header("x-real-ip","203.0.121.99").send().await.expect("independent metadata admission").status(),200);
            account_history_http(&base,&administrator,&manager,&target,observer.client()).await;
            stop.send(()).expect("stop");let (application,result)=task.await.expect("server");result.expect("drained");drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),4);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no extra supplier call through replay and drain");
            observer.terminate().await.expect("observer ended");sms.stop().await;
        }).await.expect("resumption protocol budget");
    });
}

#[test]
fn account_suspension_history_reports_read_failure_once_and_releases_resources() {
    use fixture::database_proxy::{Fault, Proxy};

    const OPERATION: &str = "00000000-0000-4000-9000-000000000101";
    const EFFECTS: &str = "SELECT jsonb_build_object('suspensions',(SELECT jsonb_agg(to_jsonb(suspension) ORDER BY id) FROM identity.suspensions suspension WHERE account_id=$1::text::uuid),'resumptions',(SELECT jsonb_agg(to_jsonb(resumption) ORDER BY id) FROM identity.resumptions resumption WHERE person_id=(SELECT person_id FROM identity.accounts WHERE id=$1::text::uuid)),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session WHERE account_id=$1::text::uuid),'revocations',(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM identity.revocations revocation WHERE account_id=$1::text::uuid),'person',(SELECT to_jsonb(person) FROM identity.persons person JOIN identity.accounts account ON account.person_id=person.id WHERE account.id=$1::text::uuid),'holder',(SELECT to_jsonb(holder) FROM access.holders holder WHERE id=$1::text::uuid))::text";

    if std::env::var_os("NUCLEUS_TEST_SUSPENSION_HISTORY").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let expectations=tempfile::NamedTempFile::new().expect("private expectations");
            let child=tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact","suspension::account_suspension_history_reports_read_failure_once_and_releases_resources","--nocapture"])
                .env("NUCLEUS_TEST_SUSPENSION_HISTORY","1").env("NUCLEUS_TEST_SUSPENSION_HISTORY_EXPECTATIONS",expectations.path())
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().expect("isolated history diagnostics child");
            let output=timeout(Duration::from_secs(60),child.wait_with_output()).await.expect("child budget").expect("child result");
            let text=String::from_utf8(output.stderr).expect("JSON diagnostics");assert!(output.status.success(),"{text}");
            let events:Vec<serde_json::Value>=text.lines().map(|line|serde_json::from_str(line).expect("JSON event")).collect();
            let failures:Vec<_>=events.iter().filter(|event|event["level"]=="ERROR").collect();
            assert_eq!(failures.len(),1,"one final read failure: {text}");
            let event=failures[0];assert_eq!(event["fields"]["stage"],"suspension_history");assert_eq!(event["fields"]["kind"],"unavailable");
            assert!(event["fields"]["error"].as_str().expect("safe technical cause").contains("Timeout("),"bounded history count retains its timeout: {event}");
            let span=event["spans"].as_array().expect("request spans").iter().find(|span|span["route"]=="/api/v1/accounts/{id}/suspensions").expect("history HTTP context");
            assert_eq!(span["method"],"GET");assert_eq!(span["operation_id"],OPERATION);
            let request_id=span["request_id"].as_str().expect("request identity");Uuid::parse_str(request_id).expect("request UUID");
            let responses:Vec<_>=events.iter().filter(|event|event["fields"]["stage"]=="http_response"&&event["spans"].as_array().is_some_and(|spans|spans.iter().any(|span|span["request_id"]==request_id))).collect();
            assert_eq!(responses.len(),1);assert_eq!(responses[0]["fields"]["status"],503);
            let tokens:Vec<String>=serde_json::from_slice(&std::fs::read(expectations.path()).expect("private expected tokens")).expect("token list");
            for secret in tokens.iter().map(String::as_str).chain(["13800000000","13900000000","Fixture Registered","Fixture Operator","00000020000229007X","000000200002290010","__Host-nucleus-session","AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","203.0.123.1",fixture::PASSWORD,"SELECT ","INSERT ","UPDATE "]) {
                assert!(!events.iter().any(|event|contains_text(event,secret)),"private material in diagnostic strings");
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(50),async {
                let name=database(&server).await;
                let supplier=Supplier::echoing().await;
                let mut sms=fixture::supplier::Supplier::start().await;
                let proxy=Proxy::start(&server).await;
                let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("drain budget"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("startup");
                let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
                let (base,stop,task)=running(application).await;
                timeout(Duration::from_secs(3),async {loop {
                    if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND usename='clerk' AND state='idle' AND query LIKE 'SELECT family, operation_id, action, target, input, subject, person_id, revision_id, created_at, expires_at, seal, seal_key_id, seal_version FROM workforce.completions WHERE expires_at <= %')",&[]).await.expect("initial empty retention scan").get::<_,bool>(0) {break;}tokio::task::yield_now().await;
                }}).await.expect("background completes its empty scan before setup and fault selection");
                let manager=issued_session_for(&base,&mut sms,"13900000000").await;
                let target=issued_session_for(&base,&mut sms,"13800000000").await;
                let candidate=account_candidate(&base,&manager,"13800000000").await;
                let account_id=candidate["identity"]["accountId"].as_str().expect("confirmed account");
                let suspension=suspend_account(&base,&manager,&candidate,"13800000000").await;
                let stored:Uuid=observer.client().query_one("SELECT suspension.id FROM identity.suspensions suspension JOIN identity.accounts account ON account.id=suspension.account_id JOIN identity.persons person ON person.id=suspension.person_id WHERE suspension.account_id=$1::text::uuid AND suspension.person_id=$2::text::uuid AND suspension.sequence=2 AND account.sequence=2 AND account.version=2 AND account.suspension_id=suspension.id AND account.changed_at=suspension.suspended_at AND person.sequence=1",&[&account_id,&candidate["identity"]["personId"].as_str().expect("fixed person")]).await.expect("independent original before any history request").get(0);
                assert_eq!(suspension["suspension"]["id"],stored.to_string());
                let original:String=observer.client().query_one(EFFECTS,&[&account_id]).await.expect("effects before read fault").get(0);
                let client=http_client();
                let path=format!("{base}/api/v1/accounts/{account_id}/suspensions");
                proxy.arm(Fault::StalledRoleCount);
                let mut caller=tokio::spawn(client.get(&path).header("operation-id",OPERATION).header("session-id",&manager.0).header("cookie",&manager.1).header("x-real-ip","203.0.123.1").timeout(Duration::from_secs(10)).send());
                let pid=timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("history count Parse held after current identity and target authorization");
                assert!(!caller.is_finished(),"count remains held before the read deadline");
                let response=timeout(Duration::from_secs(6),&mut caller).await.expect("original read deadline").expect("HTTP caller").expect("HTTP response");
                assert_eq!(response.status(),503);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure envelope"),serde_json::json!({"code":"dependency_unavailable"}));
                proxy.release();
                timeout(Duration::from_secs(3),async {loop {
                    if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",&[&pid]).await.expect("failed read backend end").get::<_,bool>(0) {break;}tokio::task::yield_now().await;
                }}).await.expect("discarded read connection ends instead of returning an open transaction to the pool");
                assert_eq!(observer.client().query_one(EFFECTS,&[&account_id]).await.expect("independent unchanged facts before retry").get::<_,String>(0),original);
                let response=client.get(&path).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&manager.0).header("cookie",&manager.1).header("x-real-ip","203.0.123.1").send().await.expect("fresh history read after technical failure");
                assert_eq!(response.status(),200);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                let page=response.json::<serde_json::Value>().await.expect("whole original page");assert_eq!(page["count"],1);assert_eq!(page["suspensions"].as_array().expect("one original suspension").len(),1);assert_eq!(page["suspensions"][0]["id"],stored.to_string());assert!(page["suspensions"][0]["resumption"].is_null());
                stop.send(()).expect("stop after failed and successful reads");let (application,result)=task.await.expect("server");result.expect("drained");drop(application);
                assert_eq!(observer.client().query_one(EFFECTS,&[&account_id]).await.expect("read and drain preserve permanent outcomes").get::<_,String>(0),original);
                assert_eq!(supplier.calls.load(Ordering::SeqCst),2);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no supplier request through fault, retry and drain");
                let tokens:Vec<_>=[&manager,&target].into_iter().map(|identity|identity.1.split_once('=').expect("cookie pair").1).collect();
                std::fs::write(std::env::var_os("NUCLEUS_TEST_SUSPENSION_HISTORY_EXPECTATIONS").expect("private expectation path"),serde_json::to_vec(&tokens).expect("token JSON")).expect("private expectations written");
                observer.terminate().await.expect("observer ended");sms.stop().await;proxy.stop().await;
            }).await.expect("history read HTTP acceptance budget");
        });
    });
}

#[test]
fn account_suspension_recovers_real_http_work_and_reports_once_after_cancellation() {
    account_state_recovery_http(AccountOperation::Suspend);
}

#[test]
fn account_resumption_recovers_real_http_work_and_reports_once_after_cancellation() {
    account_state_recovery_http(AccountOperation::Resume);
}

pub(crate) async fn account_candidate(
    base: &str,
    manager: &(String, String),
    mobile: &str,
) -> serde_json::Value {
    let response = http_client()
        .post(format!("{base}/api/v1/accounts/candidates"))
        .header("operation-id", Uuid::new_v4().to_string())
        .header("session-id", &manager.0)
        .header("cookie", &manager.1)
        .header("x-real-ip", "203.0.118.1")
        .json(&serde_json::json!({"mobile":mobile}))
        .send()
        .await
        .expect("real account confirmation");
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("set-cookie"));
    response.json().await.expect("masked account candidate")
}

pub(crate) async fn suspended_account(server: &fixture::Server, name: &str) {
    let supplier = Supplier::echoing().await;
    let mut sms = fixture::supplier::Supplier::start().await;
    let application = Application::start(
        connections(server, name, "clerk"),
        keys(0),
        services(8).expect("settings"),
        process::Settings::new(Duration::from_secs(5)).expect("stop"),
        supplier.client(),
        message_at(sms.address(), sms.authority()),
    )
    .await
    .expect("valid startup before suspension");
    let issuance = application.registration().clone();
    let (base, stop, task) = running(application).await;
    let manager = issued_session_for(&base, &mut sms, "13900000000").await;
    issued_session_for(&base, &mut sms, "13800000000").await;
    // Age the real issuance graph before suspension, so the later R1 can send again.
    age_issuance(server, name, &issuance).await;
    let candidate = account_candidate(&base, &manager, "13800000000").await;
    let response = http_client().post(format!("{base}/api/v1/accounts/{}/suspensions",candidate["identity"]["accountId"].as_str().expect("account")))
        .header("operation-id", Uuid::new_v4().to_string()).header("session-id", &manager.0).header("cookie", &manager.1)
        .json(&serde_json::json!({"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13800000000"}))
        .send().await.expect("real supervised suspension");
    assert_eq!(response.status(), 201);
    assert!(!response.headers().contains_key("set-cookie"));
    stop.send(()).expect("stop fixture");
    let (application, result) = task.await.expect("fixture task");
    result.expect("fixture drained");
    drop(application);
    assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
    assert!(
        timeout(Duration::from_millis(30), sms.request())
            .await
            .is_err(),
        "suspension makes no supplier request"
    );
    sms.stop().await;
}

pub(crate) async fn suspend_account(
    base: &str,
    manager: &(String, String),
    candidate: &serde_json::Value,
    mobile: &str,
) -> serde_json::Value {
    let response = http_client()
        .post(format!("{base}/api/v1/accounts/{}/suspensions", candidate["identity"]["accountId"].as_str().expect("confirmed account")))
        .header("operation-id", Uuid::new_v4().to_string())
        .header("session-id", &manager.0)
        .header("cookie", &manager.1)
        .header("x-real-ip", "203.0.121.1")
        .json(&serde_json::json!({"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":mobile}))
        .send().await.expect("real suspension before resumption");
    assert_eq!(response.status(), 201);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("set-cookie"));
    response.json().await.expect("permanent suspension")
}

pub(crate) async fn account_history_http(
    base: &str,
    administrator: &(String, String),
    manager: &(String, String),
    target: &(String, String),
    observer: &tokio_postgres::Client,
) {
    let account_id: Uuid = observer
        .query_one(
            "SELECT account_id FROM identity.sessions WHERE id=$1::text::uuid",
            &[&target.0],
        )
        .await
        .expect("fixed history account")
        .get(0);
    let path = format!("/api/v1/accounts/{account_id}/suspensions");
    let rows=observer.query("SELECT suspension.id,suspension.person_id,suspension.revision_id,suspension.operator_id,suspension.sequence,suspension.suspended_at,suspension.operation_id,operator.person_id AS operator_person,resumption.id AS resumption_id,resumption.revision_id AS resumed_revision,resumption.operator_id AS resumed_operator,resumed_operator.person_id AS resumed_operator_person,resumption.sequence AS resumed_sequence,resumption.resumed_at,resumption.operation_id AS resumed_operation FROM identity.suspensions suspension JOIN identity.revisions operator ON operator.id=suspension.operator_id LEFT JOIN identity.resumptions resumption ON resumption.id=suspension.id LEFT JOIN identity.revisions resumed_operator ON resumed_operator.id=resumption.operator_id WHERE suspension.account_id=$1 ORDER BY suspension.sequence DESC,suspension.id DESC",&[&account_id]).await.expect("independent permanent facts before history HTTP");
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .map(|row| row.get::<_, i64>("sequence"))
            .collect::<Vec<_>>(),
        [4, 2]
    );
    let expected:Vec<_>=rows.iter().map(|row| {
        let identity=|person:Uuid,revision:Uuid|serde_json::json!({"personId":person,"revisionId":revision,"name":"F*"});
        let resumption=row.get::<_,Option<Uuid>>("resumption_id").map(|id| {
            assert_eq!(id,row.get::<_,Uuid>("id"));
            serde_json::json!({"subject":identity(row.get("person_id"),row.get("resumed_revision")),"operator":identity(row.get("resumed_operator_person"),row.get("resumed_operator")),"sequence":row.get::<_,i64>("resumed_sequence"),"resumedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("resumed_at"),"operationId":row.get::<_,Uuid>("resumed_operation")})
        });
        serde_json::json!({"id":row.get::<_,Uuid>("id"),"subject":identity(row.get("person_id"),row.get("revision_id")),"operator":identity(row.get("operator_person"),row.get("operator_id")),"sequence":row.get::<_,i64>("sequence"),"suspendedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("suspended_at"),"operationId":row.get::<_,Uuid>("operation_id"),"resumption":resumption})
    }).collect();
    assert!(expected[0]["resumption"].is_null());
    assert_eq!(expected[1]["resumption"]["sequence"], 3);
    assert_ne!(
        expected[1]["operator"]["personId"], expected[1]["resumption"]["operator"]["personId"],
        "suspension and resumption retain their different real operators"
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("history client without implicit Origin");
    let operation = Uuid::new_v4();
    let request = |method, path: &str, identity: Option<&(String, String)>, source: u8| {
        let mut request = client
            .request(method, format!("{base}{path}"))
            .header("operation-id", operation.to_string())
            .header("x-real-ip", format!("203.0.122.{source}"));
        if let Some(identity) = identity {
            request = request
                .header("session-id", &identity.0)
                .header("cookie", &identity.1);
        }
        request
    };
    for (query, values) in [
        ("", expected.clone()),
        ("?page=1&size=1", vec![expected[0].clone()]),
        ("?page=2&size=1", vec![expected[1].clone()]),
        ("?size=100", expected.clone()),
        ("?page=3&size=1", vec![]),
        ("?page=9223372036854775807&size=1", vec![]),
    ] {
        let response = request(
            reqwest::Method::GET,
            &format!("{path}{query}"),
            Some(manager),
            3,
        )
        .header("sec-fetch-site", "same-origin")
        .send()
        .await
        .expect("authorized paused-account history");
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("set-cookie"));
        assert_eq!(
            response
                .json::<serde_json::Value>()
                .await
                .expect("complete fixed page"),
            serde_json::json!({"code":"ok","count":2,"suspensions":values})
        );
    }
    for query in [
        "?page=0",
        "?page=-1",
        "?size=0",
        "?size=101",
        "?page=",
        "?size=1.5",
        "?page=1&page=2",
        "?size=1&size=2",
        "?extra=1",
        "?personId=00000000-0000-0000-0000-000000000000",
        "?operationId=00000000-0000-0000-0000-000000000000",
        "?page=9223372036854775807&size=2",
        "?page=9223372036854775808",
    ] {
        let response = request(
            reqwest::Method::GET,
            &format!("{path}{query}"),
            Some(manager),
            4,
        )
        .header("origin", "https://example.com")
        .send()
        .await
        .expect("strict history pagination");
        assert_eq!(response.status(), 400);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("set-cookie"));
        assert_eq!(
            response
                .json::<serde_json::Value>()
                .await
                .expect("invalid pagination"),
            serde_json::json!({"code":"invalid_input"})
        );
    }
    for (builder, status, code) in [
        (client.get(format!("{base}{path}")), 400, "invalid_input"),
        (
            request(reqwest::Method::GET, &path, Some(manager), 5)
                .header("operation-id", operation.to_string()),
            400,
            "invalid_input",
        ),
        (
            request(
                reqwest::Method::GET,
                "/api/v1/accounts/not-a-uuid/suspensions",
                None,
                6,
            ),
            400,
            "invalid_input",
        ),
        (
            request(reqwest::Method::GET, &format!("{path}?size=0"), None, 7)
                .header("origin", "https://example.com"),
            401,
            "authentication_invalid",
        ),
        (
            request(reqwest::Method::GET, &path, None, 8)
                .header("cookie", &manager.1)
                .header("origin", "https://example.com"),
            401,
            "authentication_invalid",
        ),
        (
            request(reqwest::Method::GET, &path, None, 9)
                .header("session-id", &manager.0)
                .header("origin", "https://example.com"),
            401,
            "authentication_invalid",
        ),
        (
            request(reqwest::Method::GET, &path, Some(manager), 10)
                .header("session-id", &manager.0)
                .header("origin", "https://example.com"),
            400,
            "invalid_input",
        ),
        (
            request(
                reqwest::Method::GET,
                &format!("{path}?size=0"),
                Some(target),
                11,
            )
            .header("origin", "https://example.com"),
            401,
            "authentication_invalid",
        ),
        (
            request(reqwest::Method::GET, &path, Some(manager), 12),
            403,
            "permission_denied",
        ),
        (
            request(reqwest::Method::GET, &path, Some(manager), 13)
                .header("origin", "https://wrong.invalid")
                .header("sec-fetch-site", "same-origin"),
            403,
            "permission_denied",
        ),
        (
            request(reqwest::Method::GET, &path, Some(manager), 14)
                .header("origin", "https://example.com")
                .header("sec-fetch-site", "cross-site"),
            403,
            "permission_denied",
        ),
    ] {
        let response = builder.send().await.expect("history protocol boundary");
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("set-cookie"));
        assert_eq!(
            response
                .json::<serde_json::Value>()
                .await
                .expect("safe rejection"),
            serde_json::json!({"code":code})
        );
    }
    for identity in [manager, administrator] {
        let id: Uuid = observer
            .query_one(
                "SELECT account_id FROM identity.sessions WHERE id=$1::text::uuid",
                &[&identity.0],
            )
            .await
            .expect("real restricted target")
            .get(0);
        let response = request(
            reqwest::Method::GET,
            &format!("/api/v1/accounts/{id}/suspensions"),
            Some(manager),
            15,
        )
        .header("origin", "https://example.com")
        .send()
        .await
        .expect("self and higher targets hidden");
        assert_eq!(response.status(), 404);
        assert_eq!(
            response
                .json::<serde_json::Value>()
                .await
                .expect("hidden target"),
            serde_json::json!({"code":"target_unavailable"})
        );
    }
    let response = request(
        reqwest::Method::GET,
        &format!("/api/v1/accounts/{}/suspensions", Uuid::new_v4()),
        Some(manager),
        16,
    )
    .header("origin", "https://example.com")
    .send()
    .await
    .expect("missing current account");
    assert_eq!(response.status(), 404);
    assert_eq!(
        response
            .json::<serde_json::Value>()
            .await
            .expect("absent target"),
        serde_json::json!({"code":"target_unavailable"})
    );
    for (query, identity, status) in [
        ("", Some(manager), 200),
        ("?size=0", Some(manager), 400),
        ("?size=0", None, 401),
    ] {
        let response = request(
            reqwest::Method::HEAD,
            &format!("{path}{query}"),
            identity,
            17,
        )
        .header("sec-fetch-site", "same-origin")
        .send()
        .await
        .expect("history HEAD");
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(!response.headers().contains_key("set-cookie"));
        assert!(response.bytes().await.expect("HEAD has no body").is_empty());
    }
    let response = request(reqwest::Method::PUT, &path, None, 18)
        .header("session-id", "malformed-session")
        .send()
        .await
        .expect("unsupported method does not authenticate");
    assert_eq!(response.status(), 405);
    let mut allowed: Vec<_> = response.headers()["allow"]
        .to_str()
        .expect("allowed methods")
        .split(',')
        .map(str::trim)
        .collect();
    allowed.sort_unstable();
    assert_eq!(allowed, ["GET", "HEAD", "POST"]);
    assert_eq!(
        response
            .json::<serde_json::Value>()
            .await
            .expect("method envelope"),
        serde_json::json!({"code":"method_not_allowed"})
    );
    let document = client
        .get(format!("{base}/openapi/v1.json"))
        .send()
        .await
        .expect("history OpenAPI")
        .json::<serde_json::Value>()
        .await
        .expect("document");
    let route = &document["paths"]["/api/v1/accounts/{id}/suspensions"];
    assert_eq!(route["get"]["operationId"], "listAccountSuspensions");
    assert_eq!(route["post"]["operationId"], "suspendAccount");
    assert!(route["head"]["responses"]["200"].get("content").is_none());
    let parameters = route["get"]["parameters"]
        .as_array()
        .expect("history parameters");
    assert_eq!(parameters.len(), 5);
    for name in ["id", "Operation-Id", "Session-Id"] {
        assert!(
            parameters
                .iter()
                .any(|parameter| parameter["name"] == name && parameter["required"] == true)
        );
    }
    assert!(
        !parameters
            .iter()
            .any(|parameter| parameter["name"] == "Origin")
    );
    assert!(
        route["get"]["description"]
            .as_str()
            .expect("conditional Origin")
            .contains("Origin must match when supplied")
    );
    for name in ["page", "size"] {
        let parameter = parameters
            .iter()
            .find(|parameter| parameter["name"] == name)
            .expect("pagination");
        assert_eq!(parameter["required"], false);
        assert_eq!(parameter["schema"]["minimum"], 1);
    }
    assert_eq!(
        parameters
            .iter()
            .find(|parameter| parameter["name"] == "size")
            .expect("bounded size")["schema"]["maximum"],
        100
    );
    for method in ["get", "head"] {
        for status in ["200", "400", "401", "403", "404", "429", "503"] {
            assert_eq!(
                route[method]["responses"][status]["headers"]["Cache-Control"]["description"],
                "no-store"
            );
        }
        assert!(
            route[method]["responses"]["429"]["headers"]
                .get("Retry-After")
                .is_some()
        );
    }
    assert_eq!(
        route["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/suspension.history.List"
    );
    let schemas = &document["components"]["schemas"];
    for (name, fields) in [
        (
            "suspension.history.List",
            vec!["code", "count", "suspensions"],
        ),
        (
            "suspension.history.Suspension",
            vec![
                "id",
                "subject",
                "operator",
                "sequence",
                "suspendedAt",
                "operationId",
                "resumption",
            ],
        ),
        (
            "suspension.history.Resumption",
            vec![
                "subject",
                "operator",
                "sequence",
                "resumedAt",
                "operationId",
            ],
        ),
    ] {
        assert_eq!(schemas[name]["required"], serde_json::json!(fields));
        assert_eq!(
            schemas[name]["properties"]
                .as_object()
                .expect("exact disclosed fields")
                .len(),
            fields.len()
        );
    }
    assert_eq!(
        schemas["suspension.history.List"]["properties"]["suspensions"]["items"]["$ref"],
        "#/components/schemas/suspension.history.Suspension"
    );
    let nullable = &schemas["suspension.history.Suspension"]["properties"]["resumption"];
    assert!(
        nullable["oneOf"]
            .as_array()
            .expect("nullable resumption")
            .iter()
            .any(|schema| schema["type"] == "null")
    );
    assert!(
        nullable["oneOf"]
            .as_array()
            .expect("typed resumption")
            .iter()
            .any(|schema| schema["$ref"] == "#/components/schemas/suspension.history.Resumption")
    );
    for name in [
        "suspension.history.Suspension",
        "suspension.history.Resumption",
    ] {
        for field in ["subject", "operator"] {
            let reference = schemas[name]["properties"][field]["$ref"]
                .as_str()
                .expect("fixed identity schema");
            let identity = document
                .pointer(reference.strip_prefix('#').expect("local reference"))
                .expect("identity component");
            assert_eq!(
                identity["required"],
                serde_json::json!(["personId", "revisionId", "name"])
            );
            assert_eq!(
                identity["properties"]
                    .as_object()
                    .expect("masked identity only")
                    .len(),
                3
            );
        }
    }
    // Rejecting before authentication still debits the shared metadata bucket.
    timeout(Duration::from_secs(1), async {
        let mut accepted = 0;
        let endpoints = [path.as_str(), "/api/v1/accounts/current"];
        let mut limited = [false; 2];
        let mut next = 0;
        loop {
            // Alternate until one route is limited, then probe only the other one so a
            // refilled token cannot keep being consumed by the already limited route.
            let index = next;
            let endpoint = endpoints[index];
            next = match limited {
                [false, false] => 1 - index,
                [true, false] => 1,
                _ => 0,
            };
            let response = request(reqwest::Method::GET, endpoint, None, 99)
                .header("origin", "https://example.com")
                .send()
                .await
                .expect("shared metadata admission");
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            if response.status() == 429 {
                assert!(accepted >= 40);
                assert_eq!(response.headers()["retry-after"], "1");
                assert_eq!(
                    response
                        .json::<serde_json::Value>()
                        .await
                        .expect("budget envelope"),
                    serde_json::json!({"code":"rate_limited"})
                );
                limited[index] = true;
                if limited.into_iter().all(std::convert::identity) {
                    break;
                }
                next = 1 - index;
            } else {
                // A token can refill between responses, including just after a 429.
                assert_eq!(response.status(), 401);
                assert_eq!(
                    response
                        .json::<serde_json::Value>()
                        .await
                        .expect("authentication envelope after admission"),
                    serde_json::json!({"code":"authentication_invalid"})
                );
                accepted += 1;
            }
            // Two independent 40-token buckets would accept at least 80 requests.
            assert!(
                accepted < 80,
                "both routes must exhaust one shared source bucket"
            );
        }
        let response = request(reqwest::Method::POST, &path, None, 99)
            .header("origin", "https://example.com")
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("history reads leave write admission available");
        assert_eq!(response.status(), 401);
    })
    .await
    .expect("metadata observation before bucket recovery");
    let response = http_client()
        .get(format!("{base}/api/v1/accounts/current/role"))
        .header("operation-id", Uuid::new_v4().to_string())
        .header("session-id", &manager.0)
        .header("cookie", &manager.1)
        .header("x-real-ip", "203.0.122.100")
        .send()
        .await
        .expect("manager complete role");
    assert_eq!(response.status(), 200);
    let role = response.json::<serde_json::Value>().await.expect("role");
    replace_role(base, administrator, "13800000000", &role["role"], 85).await;
    let response = request(reqwest::Method::GET, &path, Some(manager), 19)
        .header("origin", "https://example.com")
        .send()
        .await
        .expect("equal-level target after a real replacement");
    assert_eq!(response.status(), 404);
    assert_eq!(
        response
            .json::<serde_json::Value>()
            .await
            .expect("equal target hidden"),
        serde_json::json!({"code":"target_unavailable"})
    );
    let response = request(reqwest::Method::GET, &path, Some(administrator), 20)
        .header("origin", "https://example.com")
        .send()
        .await
        .expect("history survives later assignment");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .json::<serde_json::Value>()
            .await
            .expect("unchanged permanent history"),
        serde_json::json!({"code":"ok","count":2,"suspensions":expected})
    );
}

pub(crate) enum AccountOperation {
    Suspend,
    Resume,
}

pub(crate) fn account_state_recovery_http(action: AccountOperation) {
    use fixture::database_proxy::{Fault, Proxy};

    let resuming = matches!(action, AccountOperation::Resume);
    let route = if resuming {
        "/api/v1/accounts/{id}/resumptions"
    } else {
        "/api/v1/accounts/{id}/suspensions"
    };
    let test = if resuming {
        "suspension::account_resumption_recovers_real_http_work_and_reports_once_after_cancellation"
    } else {
        "suspension::account_suspension_recovers_real_http_work_and_reports_once_after_cancellation"
    };
    let status = if resuming { 200 } else { 201 };
    const EFFECTS: &str = "SELECT jsonb_build_object('sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session WHERE account_id=$1::text::uuid),'revocations',(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id) FROM identity.revocations revocation WHERE account_id=$1::text::uuid),'handoffs',(SELECT jsonb_agg(to_jsonb(handoff) ORDER BY handoff.id) FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=$1::text::uuid))::text";

    struct Coordination<'a> {
        family: &'a [u8],
        operation: Uuid,
    }
    impl platform::cryptography::Input<2> for Coordination<'_> {
        fn fields(&self) -> [&[u8]; 2] {
            [self.family, self.operation.as_bytes()]
        }
    }

    if std::env::var_os("NUCLEUS_TEST_SUSPENSION_RECOVERY").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let expectations = tempfile::NamedTempFile::new().expect("private expectations");
            let child =
                tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", test, "--nocapture"])
                    .env("NUCLEUS_TEST_SUSPENSION_RECOVERY", "1")
                    .env("NUCLEUS_TEST_SUSPENSION_EXPECTATIONS", expectations.path())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .expect("isolated diagnostics child");
            let output = timeout(Duration::from_secs(90), child.wait_with_output())
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
                4,
                "one event per technical conclusion; a clean recovered commit is successful: {text}"
            );
            for index in 0..5 {
                let operation = format!("00000000-0000-4000-9000-{:012x}", index + 1);
                let matching: Vec<_> = failures
                    .iter()
                    .filter(|event| event["fields"]["operation_id"] == operation)
                    .collect();
                if index == 0 {
                    assert!(matching.is_empty(), "clean recovery needs no failure event");
                    continue;
                }
                assert_eq!(
                    matching.len(),
                    1,
                    "one final event for operation {operation}"
                );
                let event = matching[0];
                assert_eq!(event["fields"]["stage"], if matches!(index,1|4) {"completion_recovery"} else {"command"});
                if matches!(index,1|4) {
                    assert!(event["fields"]["kind"].is_null(), "known recovery reports its retained cause: {event}");
                } else {
                    assert_eq!(event["fields"]["kind"], if index==3 {"unknown"} else {"unavailable"});
                }
                let spans = event["spans"].as_array().expect("request spans");
                assert!(
                    spans.iter().any(|span| span["family"] == if resuming {"Kernel(Resumption)"} else {"Kernel(Suspension)"}
                        && span["operation_id"] == operation),
                    "exact original action: {event}"
                );
                let span = spans
                    .iter()
                    .find(|span| span["route"] == route)
                    .expect("original account-state HTTP context");
                assert_eq!(span["method"],"POST");assert_eq!(span["operation_id"],operation);
                let request_id = span["request_id"].as_str().expect("request identity");
                Uuid::parse_str(request_id).expect("request UUID");
                let error = event["fields"]["error"]
                    .as_str()
                    .expect("safe original cause");
                assert!(
                    error.contains("stage: Commit, kind: Query(connection closed)"),
                    "original write commit failure retained: {event}"
                );
                match index {
                    1 => {
                        assert_eq!(event["fields"]["outcome"], "committed");
                        assert!(error.starts_with("Inner {"),"{event}");
                        assert!(!error.contains("(Commit, "),"recovery COMMIT was acknowledged: {event}");
                        assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(),2,"original and recovery driver failures remain attached: {event}");
                    }
                    2 => {
                        assert!(event["fields"]["outcome"].is_null());
                        assert!(
                            error.starts_with("Unavailable(Inner {")
                                && !error.contains("(Commit, ")
                                && !error.contains("(Begin, "),
                            "known rollback keeps its original cause: {event}"
                        );
                        assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(),1,"recovery ended cleanly after the original driver failure: {event}");
                    }
                    3 => {
                        assert!(event["fields"]["outcome"].is_null());
                        assert!(
                            error.starts_with("Unknown(Inner {")
                                && error.contains("(Begin, Query("),
                            "failed required evidence stays unknown: {event}"
                        );
                    }
                    4 => {
                        assert_eq!(event["fields"]["outcome"], "committed");
                        assert!(error.starts_with("Inner {"),"{event}");
                        assert!(error.contains("(Commit, Query(connection closed))"),"recovery COMMIT receipt failed: {event}");
                        assert_eq!(error.matches("(Termination, Connection(database driver connection failed))").count(),2,"both driver failures remain attached after waiter cancellation: {event}");
                    }
                    _ => unreachable!("selected scenarios"),
                }
                if index != 4 {
                    let response = events
                        .iter()
                        .find(|event| {
                            event["fields"]["stage"] == "http_response"
                                && event["spans"].as_array().is_some_and(|spans| {
                                    spans.iter().any(|span| span["request_id"] == request_id)
                                })
                        })
                        .expect("response correlated to final work diagnostic");
                    assert_eq!(
                        response["fields"]["status"],
                        if index == 1 { status } else { 503 }
                    );
                }
            }
            let tokens: Vec<String> = serde_json::from_slice(
                &std::fs::read(expectations.path()).expect("private expected tokens"),
            )
            .expect("token list");
            for secret in tokens.iter().map(String::as_str).chain([
                "13800000000",
                "13900000000",
                "Fixture Registered",
                "Fixture Operator",
                "00000020000229007X",
                "000000200002290010",
                "__Host-nucleus-session",
                "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
                "fictional-sensitive-termination-detail",
                "203.0.120.1",
                fixture::PASSWORD,
                "SELECT ",
                "INSERT ",
            ]) {
                assert!(
                    !events.iter().any(|event| contains_text(event, secret)),
                    "private material in diagnostic strings"
                );
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("online diagnostics");
    isolated_child(|| {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("actual driver failure observation").block_on(async {
            timeout(Duration::from_secs(80),async {
                let mut tokens=Vec::new();
                for index in 0..5 {
                    let name=database(&server).await;
                    let supplier=Supplier::echoing().await;
                    let mut sms=fixture::supplier::Supplier::start().await;
                    let proxy=Proxy::start(&server).await;
                    let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(15)).expect("drain budget"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("startup");
                    let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
                    let (base,stop,mut task)=running(application).await;
                    timeout(Duration::from_secs(3),async {loop {
                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND usename='clerk' AND state='idle' AND query LIKE 'SELECT family, operation_id, action, target, input, subject, person_id, revision_id, created_at, expires_at, seal, seal_key_id, seal_version FROM workforce.completions WHERE expires_at <= %')",&[]).await.expect("initial empty retention scan").get::<_,bool>(0) {break;}tokio::task::yield_now().await;
                    }}).await.expect("background starts and finishes its empty scan before fault selection");
                    let manager=issued_session_for(&base,&mut sms,"13900000000").await;
                    let target=issued_session_for(&base,&mut sms,"13800000000").await;
                    for identity in [&manager,&target] {tokens.push(identity.1.split_once('=').expect("cookie pair").1.to_owned());}
                    let candidate=account_candidate(&base,&manager,"13800000000").await;
                    let account_id=candidate["identity"]["accountId"].as_str().expect("confirmed account");
                    let mut input=serde_json::json!({"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13800000000"});
                    if resuming {
                        let original=suspend_account(&base,&manager,&candidate,"13800000000").await;
                        input["suspensionId"]=original["suspension"]["id"].clone();
                    }
                    let original_effects:String=observer.client().query_one(EFFECTS,&[&account_id]).await.expect("original credential effects before fault").get(0);
                    let path=format!("/api/v1/accounts/{account_id}/{}",if resuming {"resumptions"}else{"suspensions"});
                    let operation=format!("00000000-0000-4000-9000-{:012x}",index+1);
                    let encoded=platform::cryptography::Encoded::new(&Coordination {family:if resuming {b"identity/resumption/operation"}else{b"identity/suspension/operation"},operation:Uuid::parse_str(&operation).expect("original operation")}).expect("fixed coordination encoding");
                    let digest=platform::cryptography::sha256::Digest::new(&encoded);
                    let classid=i64::from(u32::from_be_bytes(digest.as_ref()[..4].try_into().expect("lock high half")));
                    let objid=i64::from(u32::from_be_bytes(digest.as_ref()[4..8].try_into().expect("lock low half")));
                    let original_assignment:Uuid=observer.client().query_one("SELECT assignment_id FROM access.holders WHERE id=$1::text::uuid",&[&account_id]).await.expect("original assignment").get(0);
                    proxy.arm(match index {0=>Fault::LostCommit,2=>Fault::RolledBack,3=>Fault::UnknownCommit,_=>Fault::DelayedCommit});
                    let client=http_client();
                    let mut caller=tokio::spawn(client.post(format!("{base}{path}")).header("operation-id",&operation).header("session-id",&manager.0).header("cookie",&manager.1).header("x-real-ip","203.0.120.1").json(&input).timeout(Duration::from_secs(25)).send());
                    let original_pid=timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("actual account-state commit intercepted");
                    let mut stop=Some(stop);
                    if matches!(index,1|4) {
                        timeout(Duration::from_secs(3),async {loop {
                            let waiting:bool=observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity waiter JOIN pg_catalog.pg_locks waiting ON waiting.pid=waiter.pid JOIN pg_catalog.pg_stat_activity original ON original.pid=ANY(pg_catalog.pg_blocking_pids(waiter.pid)) WHERE waiter.datname=current_database() AND original.datname=current_database() AND waiting.locktype='advisory' AND NOT waiting.granted AND waiting.classid=$1::bigint::oid AND waiting.objid=$2::bigint::oid AND waiting.objsubid=1 AND waiter.query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock(%' AND original.state='idle in transaction' AND original.pid=$3)",&[&classid,&objid,&original_pid]).await.expect("recovery blocked on the exact operation by its original account-state backend").get(0);
                            if waiting {break;}tokio::task::yield_now().await;
                        }}).await.expect("recovery crosses the actual original lock");
                        let clean:bool=observer.client().query_one(if resuming {
                            "SELECT NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE operation_id=$1::text::uuid) AND EXISTS(SELECT 1 FROM identity.persons person JOIN identity.accounts account ON account.person_id=person.id JOIN identity.sessions session ON session.account_id=account.id JOIN identity.revocations revocation ON revocation.id=session.id WHERE session.id=$2::text::uuid AND person.sequence=1 AND account.sequence=2 AND account.version=2 AND revocation.mode='suspension' AND revocation.cause_id=account.suspension_id AND session.sequence=2 AND session.version=2)"
                        }else{
                            "SELECT NOT EXISTS(SELECT 1 FROM identity.suspensions WHERE operation_id=$1::text::uuid) AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE operation_id=$1::text::uuid) AND EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$2::text::uuid)"
                        },&[&operation,&target.0]).await.expect("old transaction is still uncommitted").get(0);assert!(clean);
                        assert!(!caller.is_finished());
                        if index==4 {
                            caller.abort();assert!((&mut caller).await.expect_err("caller cancellation").is_cancelled());
                            stop.take().expect("owned stop").send(()).expect("stop while original work remains owned");
                            assert!(timeout(Duration::from_millis(30),&mut task).await.is_err(),"cancelled HTTP waiter cannot release accepted work");
                        }
                        proxy.arm(if index==1 {Fault::ReadCommitTermination}else{Fault::LostReadCommit});proxy.release();
                        timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("known readback end fault");
                    }
                    let response=if index==4 {None}else{
                        let response=timeout(Duration::from_secs(15),&mut caller).await.expect("bounded original response").expect("caller").expect("HTTP result");
                        assert_eq!(response.status(),if matches!(index,2|3) {503}else{status});assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                        let response=response.json::<serde_json::Value>().await.expect("response JSON");
                        if index==2 {assert_eq!(response,serde_json::json!({"code":"dependency_unavailable"}));}
                        if index==3 {
                            assert_eq!(response,serde_json::json!({"code":"outcome_unknown"}));
                            assert_eq!(observer.client().query_one(if resuming {"SELECT count(*) FROM identity.resumptions WHERE operation_id=$1::text::uuid"}else{"SELECT count(*) FROM identity.suspensions WHERE operation_id=$1::text::uuid"},&[&operation]).await.expect("unknown before late commit").get::<_,i64>(0),0);
                        }
                        Some(response)
                    };
                    if matches!(index,0|3|4) {proxy.release();}
                    let committed=index!=2;
                    let result=timeout(Duration::from_secs(3),async {loop {
                        if resuming {
                            let option=observer.client().query_opt("SELECT resumption.id,resumption.resumed_at,resumption.id=$2::text::uuid AND suspension.account_id=$3::text::uuid AND resumption.account_id=suspension.account_id AND resumption.person_id=suspension.person_id AND resumption.revision_id=suspension.revision_id AND resumption.operator_id=suspension.operator_id AND octet_length(resumption.mobile)=48 AS bound,suspension.sequence=2 AND resumption.sequence=3 AND account.sequence=3 AND account.version=3 AND account.suspension_id IS NULL AND account.changed_at=resumption.resumed_at AND person.sequence=1 AND resumption.resumed_at>=suspension.suspended_at AND holder.assignment_id=$4 AS advanced FROM identity.resumptions resumption JOIN identity.suspensions suspension ON suspension.id=resumption.id JOIN identity.accounts account ON account.id=resumption.account_id JOIN identity.persons person ON person.id=resumption.person_id JOIN access.holders holder ON holder.id=suspension.account_id WHERE resumption.operation_id=$1::text::uuid",&[&operation,&input["suspensionId"].as_str().expect("original suspension"),&account_id,&original_assignment]).await.expect("independent exact resumption before replay or drain");
                            if let Some(row)=option {
                                assert!(committed);assert!(row.get::<_,bool>("bound"));assert!(row.get::<_,bool>("advanced"));
                                break Some(serde_json::json!({"code":"ok","resumption":{"id":row.get::<_,Uuid>("id"),"resumedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("resumed_at")}}));
                            }
                            if !committed {
                                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE operation_id=$1::text::uuid) AND EXISTS(SELECT 1 FROM identity.suspensions suspension JOIN identity.accounts account ON account.id=suspension.account_id JOIN identity.persons person ON person.id=suspension.person_id WHERE suspension.id=$2::text::uuid AND suspension.sequence=2 AND account.sequence=2 AND account.version=2 AND account.suspension_id=suspension.id AND person.sequence=1 AND NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE id=suspension.id))",&[&operation,&input["suspensionId"].as_str().expect("original suspension")]).await.expect("confirmed rollback preserves the original open suspension").get::<_,bool>(0));
                                break None;
                            }
                            tokio::task::yield_now().await;
                            continue;
                        }
                        let rows=observer.client().query("SELECT suspension.id, suspension.suspended_at, suspension.account_id=$2::text::uuid AND suspension.sequence=2 AND account.sequence=2 AND account.version=2 AND account.suspension_id=suspension.id AND account.changed_at=suspension.suspended_at AND person.sequence=1 AND holder.assignment_id=$4 AS bound, revocation.id=$3::text::uuid AND revocation.cause_id=suspension.id AND revocation.mode='suspension' AND revocation.account_id=suspension.account_id AND revocation.operation_id=suspension.operation_id AND revocation.revoked_at=suspension.suspended_at AND revocation.transaction_id=suspension.transaction_id AND revocation.sequence=2 AND session.sequence=2 AND session.version=2 AND session.revoked_at=revocation.revoked_at AS atomic, NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$3::text::uuid) AS no_handoff FROM identity.suspensions suspension JOIN identity.accounts account ON account.id=suspension.account_id JOIN identity.persons person ON person.id=suspension.person_id JOIN access.holders holder ON holder.id=suspension.account_id JOIN identity.revocations revocation ON revocation.mode='suspension' AND revocation.cause_id=suspension.id JOIN identity.sessions session ON session.id=revocation.id WHERE suspension.operation_id=$1::text::uuid",&[&operation,&account_id,&target.0,&original_assignment]).await.expect("independent operation and exact session before replay or drain");
                        if let Some(row)=rows.first() {
                            assert!(committed);assert_eq!(rows.len(),1);
                            for field in ["bound","atomic","no_handoff"] {assert!(row.get::<_,bool>(field),"{field}");}
                            break Some(serde_json::json!({"code":"ok","suspension":{"id":row.get::<_,Uuid>("id"),"suspendedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("suspended_at")}}));
                        }
                        if !committed {
                            assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.suspensions WHERE operation_id=$1::text::uuid) AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE operation_id=$1::text::uuid) AND EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$2::text::uuid) AND EXISTS(SELECT 1 FROM identity.persons person JOIN identity.accounts account ON account.person_id=person.id JOIN identity.sessions session ON session.account_id=account.id WHERE account.id=$3::text::uuid AND person.sequence=1 AND account.sequence=1 AND account.version=1 AND account.suspension_id IS NULL AND session.id=$2::text::uuid AND session.sequence=1 AND session.version=1 AND session.revoked_at IS NULL)",&[&operation,&target.0,&account_id]).await.expect("confirmed rollback leaves the complete previous graph").get::<_,bool>(0));
                            break None;
                        }
                        tokio::task::yield_now().await;
                    }}).await.expect("durable conclusion before replay");
                    if resuming {
                        let effects:String=observer.client().query_one(EFFECTS,&[&account_id]).await.expect("resumption does not change the exact credential graph").get(0);
                        assert_eq!(effects,original_effects);
                    }
                    if matches!(index,0|1) {assert_eq!(response.as_ref(),result.as_ref());}
                    if let Some(stop)=stop {stop.send(()).expect("stop original server");}
                    let (application,ended)=timeout(Duration::from_secs(15),task).await.expect("accepted work drains").expect("server task");ended.expect("normal drain");drop(application);
                    timeout(Duration::from_secs(3),async {loop {
                        if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename='clerk' AND pid<>pg_backend_pid())",&[]).await.expect("actual old backend termination").get::<_,bool>(0) {break;}tokio::task::yield_now().await;
                    }}).await.expect("all old backends ended before restart");
                    let application=Application::start(connections(&server,&name,"clerk"),keys(0),services(8).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("restart after actual termination");
                    let (base,stop,task)=running(application).await;
                    let response=http_client().post(format!("{base}{path}")).header("operation-id",&operation).header("session-id",&manager.0).header("cookie",&manager.1).json(&input).send().await.expect("same operation replay after known restart");
                    assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie"));let replay=response.json::<serde_json::Value>().await.expect("replay result");
                    if let Some(result)=result {assert_eq!(replay,result,"late or recovered original is never replaced");}
                    assert_eq!(observer.client().query_one(if resuming {"SELECT count(*) FROM identity.resumptions WHERE operation_id=$1::text::uuid"}else{"SELECT count(*) FROM identity.suspensions WHERE operation_id=$1::text::uuid"},&[&operation]).await.expect("exactly one original").get::<_,i64>(0),1);
                    let response=http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&manager.0).header("cookie",&manager.1).send().await.expect("manager session survives all target effects");assert_eq!(response.status(),200);assert!(!response.headers().contains_key("set-cookie"));
                    if resuming {
                        let effects:String=observer.client().query_one(EFFECTS,&[&account_id]).await.expect("replay preserves the exact credential graph").get(0);assert_eq!(effects,original_effects);
                        let response=http_client().get(format!("{base}/api/v1/accounts/current")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&target.0).header("cookie",&target.1).send().await.expect("resumption replay does not revive the old target session");assert_eq!(response.status(),401);assert!(!response.headers().contains_key("set-cookie"));
                    }
                    stop.send(()).expect("stop replay server");let (application,ended)=task.await.expect("replay task");ended.expect("replay drained");drop(application);
                    assert_eq!(supplier.calls.load(Ordering::SeqCst),2);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no additional supplier request through replay and drain");
                    observer.terminate().await.expect("observer ended");sms.stop().await;proxy.stop().await;
                }
                std::fs::write(std::env::var_os("NUCLEUS_TEST_SUSPENSION_EXPECTATIONS").expect("private expectation path"),serde_json::to_vec(&tokens).expect("expectation JSON")).expect("private expectations written");
            }).await.expect("account-state recovery HTTP budget");
        });
    });
}
