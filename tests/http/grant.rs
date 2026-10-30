use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*};

#[test]
fn workforce_state_management_preserves_membership_sessions_and_typed_http_contracts() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(50), async {
            let name=database(&server).await;
            let supplier=Supplier::echoing().await;let mut sms=fixture::supplier::Supplier::start().await;
            let application=Application::start(connections(&server,&name,"clerk"),keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("startup");
            let (base,stop,task)=running(application).await;
            let institution=issued_session_for(&base,&mut sms,"13900000000").await;
            let manager=issued_session_for(&base,&mut sms,"13800138000").await;
            let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source=std::cell::Cell::new(1_u8);
            let request=|path:&str,actor:&(String,String),operation:Uuid| {
                let address=source.get();source.set(address+1);
                client.post(format!("{base}{path}")).header("operation-id",operation.to_string()).header("session-id",&actor.0).header("cookie",&actor.1).header("x-real-ip",format!("203.0.147.{address}"))
            };
            let mut target_grant=serde_json::Value::Null;
            for (actor,entry,mobile,kind) in [(&institution,"institution","13800138000","full"),(&manager,"domain","13900000000","default")] {
                let response=request("/api/v1/workforce/grants/candidates",actor,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":entry,"mobile":mobile})).send().await.expect("admission candidate");assert_eq!(response.status(),200);
                let candidate=response.json::<serde_json::Value>().await.expect("candidate");
                let role=candidate["roles"].as_array().expect("choices").iter().find(|role|role["kind"]==kind).expect("selected system role");
                let response=request("/api/v1/workforce/grants",actor,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":entry,"target":{
                    "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":mobile,
                    "assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
                },"role":{"id":role["id"],"changeId":role["changeId"]}})).send().await.expect("real admission");assert_eq!(response.status(),201);
                target_grant=response.json::<serde_json::Value>().await.expect("grant result");
            }
            let candidate_path="/api/v1/workforce/suspensions/candidates";
            for (builder,status) in [
                (request(candidate_path,&manager,Uuid::new_v4()).json(&serde_json::json!({"mobile":"13900000000"})),403),
                (request(candidate_path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800138000"})),403),
                (request(candidate_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800138000"})),404),
                (request(candidate_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"invalid"})),422),
                (request(&format!("{candidate_path}?target=other"),&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13900000000"})),400),
            ] { let response=builder.send().await.expect("candidate boundary");assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie")); }
            let response=request(candidate_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13900000000"})).send().await.expect("suspension candidate");assert_eq!(response.status(),200);
            let candidate=response.json::<serde_json::Value>().await.expect("masked target");
            assert_eq!(candidate["grantId"],target_grant["grant"]["id"]);assert_eq!(candidate.as_object().expect("candidate shape").len(),3);
            let body=serde_json::json!({"target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13900000000"}});
            let id=candidate["grantId"].as_str().expect("grant ID");let path=format!("/api/v1/workforce/grants/{id}/suspensions");
            let mut invalid=body.clone();invalid["target"]["revisionId"]=serde_json::json!("not-a-uuid");
            let mut stale=body.clone();stale["target"]["revisionId"]=serde_json::json!(Uuid::new_v4());
            let mut cadence=tokio::time::interval(Duration::from_millis(150));
            cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            for (builder,status,code) in [
                (request(&path,&manager,Uuid::new_v4()).json(&body),403,"permission_denied"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").header("sec-fetch-site","cross-site").json(&body),403,"permission_denied"),
                (request(&path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&body),403,"permission_denied"),
                (request(&path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&invalid),422,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&stale),409,"target_changed"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({})),422,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").header("content-type","application/json").body("null"),422,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").body(body.to_string()),415,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").header("content-type","application/json").body("{"),400,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
                (request("/api/v1/workforce/grants/not-a-uuid/suspensions",&manager,Uuid::new_v4()).header("origin","https://example.com").json(&body),400,"invalid_input"),
                (request(&format!("{path}?other=true"),&manager,Uuid::new_v4()).header("origin","https://example.com").json(&body),400,"invalid_input"),
            ] { cadence.tick().await;let response=builder.send().await.expect("typed suspension boundary");assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("safe rejection"),serde_json::json!({"code":code})); }
            let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
            assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM access.suspensions) AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=2",&[]).await.expect("rejections do not pause or remove membership").get::<_,bool>(0));
            let operation=Uuid::new_v4();
            let response=request(&path,&manager,operation).header("origin","https://example.com").json(&body).send().await.expect("suspension");assert_eq!(response.status(),201);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
            let result=response.json::<serde_json::Value>().await.expect("suspension receipt");
            assert_eq!(result["suspension"]["grantId"],id);assert_eq!(result["suspension"]["sequence"],2);assert_ne!(result["suspension"]["id"],id);
            assert_eq!(result["suspension"]["subject"],target_grant["grant"]["subject"]);assert_eq!(result["suspension"]["operator"],target_grant["grant"]["operator"]);
            assert_eq!(result["suspension"]["subject"].as_object().expect("fixed masked identity").len(),3);
            assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.suspensions)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=2 AND NOT EXISTS(SELECT 1 FROM access.terminations) AND EXISTS(SELECT 1 FROM access.suspensions suspension JOIN identity.persons person ON person.id=suspension.person_id WHERE suspension.operation_id=$1 AND person.sequence=1 AND suspension.sequence=2 AND octet_length(suspension.seal)=32)",&[&operation]).await.expect("durable complete state before replay").get::<_,bool>(0));
            let response=request(&path,&manager,operation).header("origin","https://example.com").json(&body).send().await.expect("original replay");assert_eq!(response.status(),201);assert_eq!(response.json::<serde_json::Value>().await.expect("original receipt"),result);
            let response=request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&body).send().await.expect("already suspended");assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("state conflict")["code"],"state_conflict");
            let mut changed=body.clone();changed["target"]["mobile"]=serde_json::json!("13700000000");
            let response=request(&path,&manager,operation).header("origin","https://example.com").json(&changed).send().await.expect("altered original");assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch")["code"],"request_mismatch");
            let candidate_path="/api/v1/workforce/resumptions/candidates";
            for (builder,status,code) in [
                (request(candidate_path,&manager,Uuid::new_v4()).json(&serde_json::json!({"mobile":"13900000000"})),403,"permission_denied"),
                (request(candidate_path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13900000000"})),403,"permission_denied"),
                (request(candidate_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800138000"})),404,"target_unavailable"),
                (request(candidate_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"invalid"})),422,"invalid_input"),
                (request(&format!("{candidate_path}?other=true"),&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13900000000"})),400,"invalid_input"),
            ] { cadence.tick().await;let response=builder.send().await.expect("resumption candidate boundary");assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("safe candidate rejection"),serde_json::json!({"code":code})); }
            let response=request(candidate_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13900000000"})).send().await.expect("exact resumption candidate");assert_eq!(response.status(),200);
            let confirmed=response.json::<serde_json::Value>().await.expect("resumption confirmation");
            assert_eq!(confirmed["identity"],candidate["identity"]);assert_eq!(confirmed["suspensionId"],result["suspension"]["id"]);assert_eq!(confirmed.as_object().expect("candidate shape").len(),3);
            let paused=confirmed["suspensionId"].as_str().expect("suspension ID");let resume_path=format!("/api/v1/workforce/suspensions/{paused}/resumptions");
            for (builder,status,code) in [
                (request(&resume_path,&manager,Uuid::new_v4()).json(&body),403,"permission_denied"),
                (request(&resume_path,&manager,Uuid::new_v4()).header("origin","https://example.com").header("sec-fetch-site","cross-site").json(&body),403,"permission_denied"),
                (request(&resume_path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&body),403,"permission_denied"),
                (request(&resume_path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&invalid),422,"invalid_input"),
                (request(&resume_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&stale),409,"target_changed"),
                (request(&resume_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"target":body["target"],"extra":true})),422,"invalid_input"),
                (request(&resume_path,&manager,Uuid::new_v4()).header("origin","https://example.com").body(body.to_string()),415,"invalid_input"),
                (request(&resume_path,&manager,Uuid::new_v4()).header("origin","https://example.com").header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
                (request("/api/v1/workforce/suspensions/not-a-uuid/resumptions",&manager,Uuid::new_v4()).header("origin","https://example.com").json(&body),400,"invalid_input"),
                (request(&format!("{resume_path}?other=true"),&manager,Uuid::new_v4()).header("origin","https://example.com").json(&body),400,"invalid_input"),
            ] { cadence.tick().await;let response=builder.send().await.expect("resumption protocol boundary");assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("safe resumption rejection"),serde_json::json!({"code":code})); }
            assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM access.resumptions) AND (SELECT count(*) FROM access.suspensions)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=2",&[]).await.expect("rejections retain the open pause").get::<_,bool>(0));
            let resume_operation=Uuid::new_v4();
            let response=request(&resume_path,&manager,resume_operation).header("origin","https://example.com").json(&body).send().await.expect("actual resumption");assert_eq!(response.status(),201);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
            let resumed=response.json::<serde_json::Value>().await.expect("resumption receipt");
            assert_eq!(resumed["resumption"]["id"],paused);assert_eq!(resumed["resumption"]["grantId"],id);assert_eq!(resumed["resumption"]["sequence"],3);assert_eq!(resumed["resumption"]["operationId"],resume_operation.to_string());
            assert_eq!(resumed["resumption"]["subject"],result["suspension"]["subject"]);assert_eq!(resumed["resumption"]["operator"],result["suspension"]["operator"]);
            assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.resumptions)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=2 AND NOT EXISTS(SELECT 1 FROM identity.revocations) AND EXISTS(SELECT 1 FROM access.resumptions resumption JOIN identity.persons person ON person.id=resumption.person_id WHERE resumption.operation_id=$1 AND resumption.sequence=3 AND person.sequence=1)",&[&resume_operation]).await.expect("independent committed resumption before replay").get::<_,bool>(0));
            let response=client.get(format!("{base}/api/v1/workforce/current/grant")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&institution.0).header("cookie",&institution.1).header("origin","https://example.com").header("x-real-ip","203.0.147.201").send().await.expect("old session reads resumed membership");assert_eq!(response.status(),200);assert_eq!(response.json::<serde_json::Value>().await.expect("active projection")["grant"]["status"],"active");
            let response=request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&body).send().await.expect("pause again");assert_eq!(response.status(),201);
            let later=response.json::<serde_json::Value>().await.expect("later suspension");assert_ne!(later["suspension"]["id"],paused);assert_eq!(later["suspension"]["sequence"],4);
            let response=request(&resume_path,&manager,resume_operation).header("origin","https://example.com").json(&body).send().await.expect("old resumption replay");assert_eq!(response.status(),201);assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("fixed original"),resumed);
            let response=request(&resume_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&body).send().await.expect("old pause cannot clear the new one");assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("state conflict")["code"],"state_conflict");
            assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.resumptions)=1 AND (SELECT count(*) FROM access.suspensions)=2 AND EXISTS(SELECT 1 FROM identity.persons person JOIN access.suspensions suspension ON suspension.person_id=person.id WHERE person.sequence=1 AND suspension.sequence=4 AND NOT EXISTS(SELECT 1 FROM access.resumptions WHERE id=suspension.id))",&[]).await.expect("old replay preserves the new open suspension").get::<_,bool>(0));
            for path in ["/api/v1/accounts/current","/api/v1/workforce/current/grant","/api/v1/workforce/current/role"] {
                let response=client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&institution.0).header("cookie",&institution.1).header("origin","https://example.com").header("x-real-ip","203.0.147.200").send().await.expect("retained holder reads");assert_eq!(response.status(),200);assert!(!response.headers().contains_key("set-cookie"));
                if path.ends_with("grant") { assert_eq!(response.json::<serde_json::Value>().await.expect("current projection")["grant"]["status"],"suspended"); }
            }
            let response=request(&format!("/api/v1/workforce/grants/{id}/terminations"),&institution,Uuid::new_v4()).header("origin","https://example.com").send().await.expect("self exit during domain suspension");assert_eq!(response.status(),201);
            let response=request(&path,&manager,operation).header("origin","https://example.com").json(&body).send().await.expect("original pause after exit");assert_eq!(response.status(),201);assert_eq!(response.json::<serde_json::Value>().await.expect("retained original"),result);
            let schema=client.get(format!("{base}/openapi/v1.json")).send().await.expect("schema").json::<serde_json::Value>().await.expect("OpenAPI");
            let endpoint=&schema["paths"]["/api/v1/workforce/grants/{id}/suspensions"]["post"];
            assert_eq!(endpoint["requestBody"]["required"],true);assert_eq!(endpoint["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/grant.suspension.Input");
            assert!(endpoint["parameters"].as_array().expect("headers").iter().any(|parameter|parameter["name"]=="Origin" && parameter["required"]==true));
            assert_eq!(schema["components"]["schemas"]["grant.Status"]["enum"],serde_json::json!(["active","suspended","terminated"]));
            assert_eq!(schema["components"]["schemas"]["grant.suspension.Suspension"]["properties"]["id"]["$ref"],"#/components/schemas/grant.suspension.Id");
            let endpoint=&schema["paths"]["/api/v1/workforce/suspensions/{id}/resumptions"]["post"];
            assert_eq!(endpoint["requestBody"]["required"],true);assert_eq!(endpoint["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/grant.resumption.Input");
            assert!(endpoint["parameters"].as_array().expect("resumption headers").iter().any(|parameter|parameter["name"]=="Origin" && parameter["required"]==true));
            assert_eq!(schema["components"]["schemas"]["grant.resumption.Resumption"]["properties"]["id"]["$ref"],"#/components/schemas/grant.suspension.Id");
            assert!(schema["components"]["schemas"]["grant.resumption.Candidate"]["required"].as_array().expect("required exact pause").iter().any(|field|field=="suspensionId"));
            observer.terminate().await.expect("observer ended");stop.send(()).expect("stop");let(application,ended)=task.await.expect("process");ended.expect("drained");drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err());sms.stop().await;
        }).await.expect("suspension HTTP deadline");
    });
}

#[test]
fn workforce_managed_termination_uses_domain_authority_and_typed_confirmation() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(50), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server,&name,"clerk"),keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("startup");
            let (base,stop,task)=running(application).await;
            let institution=issued_session_for(&base,&mut sms,"13900000000").await;
            let manager=issued_session_for(&base,&mut sms,"13800138000").await;
            let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source=std::cell::Cell::new(1_u8);
            let request=|path:&str,actor:&(String,String),operation:Uuid| {
                let address=source.get();source.set(address+1);
                client.post(format!("{base}{path}")).header("operation-id",operation.to_string()).header("session-id",&actor.0).header("cookie",&actor.1).header("x-real-ip",format!("203.0.146.{address}"))
            };
            let mut target_grant=serde_json::Value::Null;
            for (actor,entry,mobile,kind) in [(&institution,"institution","13800138000","full"),(&manager,"domain","13900000000","default")] {
                let response=request("/api/v1/workforce/grants/candidates",actor,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":entry,"mobile":mobile})).send().await.expect("admission candidate");
                assert_eq!(response.status(),200);
                let candidate=response.json::<serde_json::Value>().await.expect("candidate");
                let role=candidate["roles"].as_array().expect("choices").iter().find(|role|role["kind"]==kind).expect("selected system role");
                let response=request("/api/v1/workforce/grants",actor,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":entry,"target":{
                    "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":mobile,
                    "assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
                },"role":{"id":role["id"],"changeId":role["changeId"]}})).send().await.expect("real admission");
                assert_eq!(response.status(),201);
                target_grant=response.json::<serde_json::Value>().await.expect("grant result");
            }
            let candidate_path="/api/v1/workforce/terminations/candidates";
            for (builder,status) in [
                (request(candidate_path,&manager,Uuid::new_v4()).json(&serde_json::json!({"mobile":"13900000000"})),403),
                (request(candidate_path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800138000"})),403),
                (request(candidate_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13800138000"})),404),
                (request(candidate_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"invalid"})),422),
                (request(&format!("{candidate_path}?target=other"),&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13900000000"})),400),
            ] { let response=builder.send().await.expect("candidate boundary");assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie")); }
            let response=request(candidate_path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"mobile":"13900000000"})).send().await.expect("termination candidate");
            assert_eq!(response.status(),200);
            let candidate=response.json::<serde_json::Value>().await.expect("masked target");
            assert_eq!(candidate["grantId"],target_grant["grant"]["id"]);
            assert!(candidate["identity"]["name"].as_str().expect("masked name").ends_with('*'));
            assert_eq!(candidate.as_object().expect("candidate shape").len(),3);
            let body=serde_json::json!({"target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13900000000"}});
            let id=candidate["grantId"].as_str().expect("grant ID");
            let path=format!("/api/v1/workforce/grants/{id}/terminations");
            let mut invalid=body.clone();invalid["target"]["accountId"]=serde_json::json!("not-a-uuid");
            let mut stale=body.clone();stale["target"]["revisionId"]=serde_json::json!(Uuid::new_v4());
            // Keep protocol rejections below the shared authenticated-write rate.
            let mut cadence=tokio::time::interval(Duration::from_millis(150));
            cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            for (builder,status,code) in [
                (request(&path,&manager,Uuid::new_v4()).json(&body),403,"permission_denied"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").header("sec-fetch-site","cross-site").json(&body),403,"permission_denied"),
                (request(&path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&body),403,"permission_denied"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&invalid),422,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&stale),409,"target_changed"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({})),422,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").header("content-type","application/json").body("null"),422,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").body(body.to_string()),415,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").header("content-type","application/json").body("{"),400,"invalid_input"),
                (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com").header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
            ] {
                cadence.tick().await;
                let response=builder.send().await.expect("typed management boundary");assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe rejection"),serde_json::json!({"code":code}));
            }
            let operation=Uuid::new_v4();
            let response=request(&path,&manager,operation).header("origin","https://example.com").json(&body).send().await.expect("managed termination");
            assert_eq!(response.status(),201);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
            let result=response.json::<serde_json::Value>().await.expect("termination receipt");
            assert_eq!(result["termination"]["mode"],"manager");assert_eq!(result["termination"]["id"],id);assert_eq!(result["termination"]["sequence"],2);
            assert_eq!(result["termination"]["subject"],target_grant["grant"]["subject"]);assert_eq!(result["termination"]["operator"],target_grant["grant"]["operator"]);
            assert_eq!(result["termination"]["subject"].as_object().expect("fixed masked identity").len(),3);
            let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
            assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.terminations)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=1 AND EXISTS(SELECT 1 FROM access.terminations termination JOIN identity.persons person ON person.id=termination.person_id WHERE termination.operation_id=$1 AND person.sequence=1 AND termination.sequence=2 AND octet_length(termination.seal)=32)",&[&operation]).await.expect("durable fact before replay").get::<_,bool>(0));
            let response=request(&path,&manager,operation).header("origin","https://example.com").json(&body).send().await.expect("original replay");assert_eq!(response.status(),201);assert_eq!(response.json::<serde_json::Value>().await.expect("original receipt"),result);
            let mut changed=body.clone();changed["target"]["mobile"]=serde_json::json!("13700000000");
            let response=request(&path,&manager,operation).header("origin","https://example.com").json(&changed).send().await.expect("altered original");assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch")["code"],"request_mismatch");
            for actor in [&manager,&institution] {
                let response=client.get(format!("{base}/api/v1/accounts/current")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&actor.0).header("cookie",&actor.1).header("origin","https://example.com").header("x-real-ip","203.0.146.200").send().await.expect("both sessions preserved");assert_eq!(response.status(),200);assert!(!response.headers().contains_key("set-cookie"));
            }
            let schema=client.get(format!("{base}/openapi/v1.json")).send().await.expect("schema").json::<serde_json::Value>().await.expect("OpenAPI");
            assert_eq!(schema["paths"][candidate_path]["post"]["requestBody"]["required"],true);
            for name in ["accountId","personId","revisionId","mobile"] { assert!(schema["components"]["schemas"]["grant.member.Target"]["required"].as_array().expect("required target").contains(&serde_json::json!(name))); }
            assert_eq!(schema["components"]["schemas"]["grant.member.Target"]["additionalProperties"],false);
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop");let(application,ended)=task.await.expect("process");ended.expect("drained");drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err());sms.stop().await;
        }).await.expect("managed termination HTTP deadline");
    });
}

#[test]
fn workforce_self_termination_keeps_the_session_and_preserves_rejoined_membership() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(40), async {
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
            let holder = issued_session_for(&base, &mut sms, "13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = std::cell::Cell::new(1_u8);
            let request = |path: &str, identity: &(String,String), operation: Uuid| {
                let address = source.get(); source.set(address + 1);
                client.post(format!("{base}{path}"))
                    .header("operation-id", operation.to_string()).header("session-id", &identity.0).header("cookie", &identity.1)
                    .header("x-real-ip", format!("203.0.138.{address}"))
            };
            let mut original = serde_json::Value::Null;
            let mut original_id = String::new();
            let operation = Uuid::new_v4();
            let observer = connections(&server,&name,"clerk").recover().await.expect("independent observer");
            for rejoining in [false, true] {
                let response = request("/api/v1/workforce/grants/candidates", &manager, Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("real candidate");
                assert_eq!(response.status(),200);
                let candidate = response.json::<serde_json::Value>().await.expect("candidate");
                let response = request("/api/v1/workforce/grants", &manager, Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","target":{
                    "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                    "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
                },"role":{"id":candidate["roles"][1]["id"],"changeId":candidate["roles"][1]["changeId"]}})).send().await.expect("admission");
                assert_eq!(response.status(),201);
                let admission = response.json::<serde_json::Value>().await.expect("admission result");
                let id = admission["grant"]["id"].as_str().expect("grant id");
                if !rejoining { original_id = id.to_owned(); }
                let path = format!("/api/v1/workforce/grants/{original_id}/terminations");
                if !rejoining {
                    for (builder,status,code) in [
                        (request(&path,&holder,Uuid::new_v4()),403,"permission_denied"),
                        (request(&path,&holder,Uuid::new_v4()).header("origin","https://wrong.invalid"),403,"permission_denied"),
                        (request(&path,&holder,Uuid::new_v4()).header("origin","https://example.com").header("sec-fetch-site","cross-site"),403,"permission_denied"),
                        (request(&path,&manager,Uuid::new_v4()).header("origin","https://example.com"),404,"target_unavailable"),
                        (request("/api/v1/workforce/grants/invalid/terminations",&holder,Uuid::new_v4()).header("origin","https://example.com"),400,"invalid_input"),
                        (request(&format!("{path}?target=other"),&holder,Uuid::new_v4()).header("origin","https://example.com"),400,"invalid_input"),
                        (request(&path,&holder,Uuid::new_v4()).header("origin","https://example.com").body("{}"),415,"invalid_input"),
                    ] {
                        let response = builder.send().await.expect("public boundary"); assert_eq!(response.status(),status);
                        assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                        assert_eq!(response.json::<serde_json::Value>().await.expect("safe rejection"),serde_json::json!({"code":code}));
                    }
                }
                let response = request(&path,&holder,operation).header("origin","https://example.com").send().await.expect("self exit or original replay");
                assert_eq!(response.status(),201); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                let result = response.json::<serde_json::Value>().await.expect("original termination");
                if rejoining { assert_eq!(result,original); }
                else {
                    assert_eq!(result["code"],"ok"); assert_eq!(result["termination"]["id"],original_id);
                    assert_eq!(result["termination"]["mode"],"self"); assert_eq!(result["termination"]["sequence"],2);
                    assert_eq!(result["termination"]["operationId"],operation.to_string());
                    assert_eq!(result["termination"]["subject"],result["termination"]["operator"]);
                    assert_eq!(result["termination"]["subject"],admission["grant"]["subject"]);
                    assert_eq!(result["termination"]["subject"].as_object().expect("masked identity").len(),3);
                    original = result;
                    let response = request(&path,&holder,Uuid::new_v4()).header("origin","https://example.com").send().await.expect("already ended");
                    assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("state conflict")["code"],"state_conflict");
                }
                for route in ["/api/v1/accounts/current","/api/v1/workforce/current/grant","/api/v1/workforce/current/role"] {
                    let response = client.get(format!("{base}{route}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&holder.0).header("cookie",&holder.1).header("origin","https://example.com").header("x-real-ip","203.0.138.2").send().await.expect("same session still works");
                    assert_eq!(response.status(),if route.ends_with("/role") && !rejoining {404} else {200});
                    assert!(!response.headers().contains_key("set-cookie"));
                    let result = response.json::<serde_json::Value>().await.expect("current projection");
                    if route.ends_with("/grant") { assert_eq!(result["grant"]["id"],id); assert_eq!(result["grant"]["status"],if rejoining {"active"} else {"terminated"}); }
                }
                let person: Uuid = candidate["identity"]["personId"].as_str().expect("person").parse().expect("UUID");
                let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.terminations),(SELECT count(*) FROM access.grants WHERE termination IS NULL),sequence AS person_sequence FROM identity.persons WHERE id=$1", &[&person]).await.expect("independent permanent state after HTTP replay");
                assert_eq!(row.get::<_,i64>(0),1); assert_eq!(row.get::<_,i64>(1),i64::from(rejoining)); assert_eq!(row.get::<_,i64>(2),1);
            }
            let schema = client.get(format!("{base}/openapi/v1.json")).send().await.expect("schema").json::<serde_json::Value>().await.expect("OpenAPI");
            let route = &schema["paths"]["/api/v1/workforce/grants/{id}/terminations"]["post"];
            assert_eq!(route["requestBody"]["required"],false, "body remains optional for self exit");
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"],serde_json::json!({"$ref":"#/components/schemas/termination.Input"}),"supplied JSON is the required object, never null");
            for name in ["id","Operation-Id","Session-Id","Origin"] { assert_eq!(route["parameters"].as_array().expect("parameters").iter().find(|item|item["name"]==name).expect("required parameter")["required"],true); }
            assert_eq!(schema["components"]["schemas"]["termination.Mode"]["enum"],serde_json::json!(["self","manager","cancellation"]));
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application,ended)=task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);
            assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no external effect through replay and drain"); sms.stop().await;
        }).await.expect("HTTP self termination deadline");
    });
}

#[test]
fn workforce_admission_uses_real_http_confirmation_and_one_permanent_result() {
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
            let (base, stop, task) = running(application).await;
            let manager = issued_session_for(&base, &mut sms, "13900000000").await;
            let ordinary = issued_session_for(&base, &mut sms, "13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let request = |path: &str, identity: &(String,String), operation: Uuid, source: u8| client.post(format!("{base}{path}"))
                .header("operation-id", operation.to_string()).header("session-id", &identity.0).header("cookie", &identity.1)
                .header("x-real-ip", format!("203.0.135.{source}"));
            let search = serde_json::json!({"entry":"institution","mobile":"13800138000"});
            let path = "/api/v1/workforce/grants/candidates";
            for (builder, status, code) in [
                (request(path,&manager,Uuid::new_v4(),1).json(&search),403,"permission_denied"),
                (request(path,&manager,Uuid::new_v4(),2).header("origin","https://wrong.invalid").json(&search),403,"permission_denied"),
                (request(path,&ordinary,Uuid::new_v4(),3).header("origin","https://example.com").json(&search),403,"permission_denied"),
                (request(path,&manager,Uuid::new_v4(),4).header("origin","https://example.com").json(&serde_json::json!({"entry":"domain","mobile":"13800138000"})),403,"permission_denied"),
                (request(path,&manager,Uuid::new_v4(),5).header("origin","https://example.com").header("sec-fetch-site","cross-site").json(&search),403,"permission_denied"),
                (request(path,&manager,Uuid::new_v4(),6).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","mobile":"13900000000"})),404,"target_unavailable"),
            ] {
                let response = builder.send().await.expect("actual boundary"); assert_eq!(response.status(),status);
                assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure"),serde_json::json!({"code":code}));
            }
            let response = request(path,&manager,Uuid::new_v4(),7).header("origin","https://example.com").json(&search).send().await.expect("authorized candidate");
            assert_eq!(response.status(),200);
            let candidate = response.json::<serde_json::Value>().await.expect("candidate");
            assert_eq!(candidate["identity"]["name"],"F*");
            assert_eq!(candidate["identity"]["idNumber"],serde_json::json!({"suffix":"007X"}));
            assert_eq!(candidate["identity"].as_object().expect("masked identity").len(),5);
            assert!(candidate["suspensionId"].is_null() && candidate["grant"].is_null());
            assert_eq!(candidate["roles"].as_array().expect("system choices").len(),2);
            assert_eq!(candidate["count"],2);
            assert_eq!(candidate["roles"][0]["level"],1000); assert_eq!(candidate["roles"][1]["level"],1);
            assert_eq!(candidate["roles"][0]["permissions"].as_array().expect("all Workforce powers").len(),11);
            let role = &candidate["roles"][0];
            let body = serde_json::json!({"entry":"institution","target":{
                "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":null,"grant":null
            },"role":{"id":role["id"],"changeId":role["changeId"]}});
            let operation = Uuid::new_v4();
            let response = request("/api/v1/workforce/grants",&manager,operation,8).header("origin","https://example.com").json(&body).send().await.expect("admit");
            assert_eq!(response.status(),201); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let original = response.json::<serde_json::Value>().await.expect("grant and assignment");
            assert_eq!(original["grant"]["status"],"active"); assert_eq!(original["grant"]["sequence"],1);
            assert_eq!(original["assignment"]["grantId"],original["grant"]["id"]);
            assert_eq!(original["assignment"]["role"],*role);
            assert_eq!(original["grant"]["subject"]["personId"],candidate["identity"]["personId"]);
            let response = request("/api/v1/workforce/grants",&manager,operation,9).header("origin","https://example.com").json(&body).send().await.expect("replay");
            assert_eq!(response.status(),201); assert_eq!(response.json::<serde_json::Value>().await.expect("same result"),original);
            let mut wrong = body.clone(); wrong["role"]["changeId"] = serde_json::json!(Uuid::new_v4());
            let response = request("/api/v1/workforce/grants",&manager,operation,10).header("origin","https://example.com").json(&wrong).send().await.expect("mismatch");
            assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch code"),serde_json::json!({"code":"request_mismatch"}));
            wrong = body.clone(); wrong["target"]["accountId"] = serde_json::json!("invalid");
            let response = request("/api/v1/workforce/grants",&ordinary,Uuid::new_v4(),11).header("origin","https://example.com").json(&wrong).send().await.expect("strong input precedes authority");
            assert_eq!(response.status(),422);
            let response = request(path,&ordinary,Uuid::new_v4(),12).header("origin","https://example.com").json(&search).send().await.expect("Workforce administrator does not gain institution powers");
            assert_eq!(response.status(),403);
            let mut occupied = body;
            occupied["target"]["grant"] = serde_json::json!({"id":original["grant"]["id"],"status":"active"});
            let response = request("/api/v1/workforce/grants",&manager,Uuid::new_v4(),13).header("origin","https://example.com").json(&occupied).send().await.expect("already admitted");
            assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("conflict"),serde_json::json!({"code":"already_authorized"}));
            let observer = connections(&server,&name,"clerk").recover().await.expect("independent runtime observer");
            let person: Uuid = candidate["identity"]["personId"].as_str().expect("person").parse().expect("UUID");
            assert!(observer.client().query_one("SELECT sequence=1 AND (SELECT count(*) FROM access.grants)=1 AND (SELECT count(*) FROM access.assignments)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=1 FROM identity.persons WHERE id=$1", &[&person]).await.expect("one permanent admission").get::<_,bool>(0));
            let response = request(path,&ordinary,Uuid::new_v4(),14).header("origin","https://example.com").json(&serde_json::json!({"entry":"domain","mobile":"13900000000"})).send().await.expect("domain candidate");
            assert_eq!(response.status(),200);
            let domain = response.json::<serde_json::Value>().await.expect("masked domain confirmation");
            assert_eq!(domain["identity"].as_object().expect("masked identity").len(),5);
            assert_eq!(domain["roles"].as_array().expect("eligible roles").len(),1);
            assert_eq!(domain["roles"][0]["level"],1);
            let body = serde_json::json!({"entry":"domain","target":{
                "accountId":domain["identity"]["accountId"],"personId":domain["identity"]["personId"],"revisionId":domain["identity"]["revisionId"],
                "mobile":"13900000000","assignmentId":domain["assignmentId"],"suspensionId":domain["suspensionId"],"grant":domain["grant"]
            },"role":{"id":domain["roles"][0]["id"],"changeId":domain["roles"][0]["changeId"]}});
            let mut elevated = body.clone();
            elevated["role"] = serde_json::json!({"id":role["id"],"changeId":role["changeId"]});
            let response = request("/api/v1/workforce/grants",&ordinary,Uuid::new_v4(),15).header("origin","https://example.com").json(&elevated).send().await.expect("domain cannot assign full role");
            assert_eq!(response.status(),403);
            assert_eq!(response.json::<serde_json::Value>().await.expect("safe denial"),serde_json::json!({"code":"permission_denied"}));
            let operation = Uuid::new_v4();
            let response = request("/api/v1/workforce/grants",&ordinary,operation,16).header("origin","https://example.com").json(&body).send().await.expect("domain admission");
            assert_eq!(response.status(),201); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let original = response.json::<serde_json::Value>().await.expect("domain result");
            assert_eq!(original["assignment"]["role"],domain["roles"][0]);
            assert_eq!(original["grant"]["operator"]["personId"],candidate["identity"]["personId"]);
            assert_eq!(original["grant"]["subject"]["personId"],domain["identity"]["personId"]);
            let response = request("/api/v1/workforce/grants",&ordinary,operation,17).header("origin","https://example.com").json(&body).send().await.expect("domain replay");
            assert_eq!(response.status(),201); assert_eq!(response.json::<serde_json::Value>().await.expect("exact original"),original);
            let response = request(path,&manager,Uuid::new_v4(),18).header("origin","https://example.com").json(&serde_json::json!({"entry":"unsupported","mobile":"13800138000"})).send().await.expect("closed entry vocabulary");
            assert_eq!(response.status(),422); assert_eq!(response.json::<serde_json::Value>().await.expect("invalid entry"),serde_json::json!({"code":"invalid_input"}));
            assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.grants)=2 AND (SELECT count(*) FROM access.assignments)=2 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=2 AND (SELECT count(*) FROM access.grants WHERE entry='institution')=1 AND (SELECT count(*) FROM access.grants WHERE entry='domain')=1 AND NOT EXISTS(SELECT 1 FROM identity.persons WHERE sequence<>1)", &[]).await.expect("two explicit admissions without replay writes").get::<_,bool>(0));
            let schema = client.get(format!("{base}/openapi/v1.json")).send().await.expect("schema").json::<serde_json::Value>().await.expect("OpenAPI");
            for path in ["/api/v1/workforce/grants", "/api/v1/workforce/grants/candidates"] {
                let params = schema["paths"][path]["post"]["parameters"].as_array().expect("documented headers");
                for name in ["Operation-Id","Session-Id","Origin"] { assert_eq!(params.iter().find(|item|item["name"]==name).expect("required header")["required"],true); }
            }
            assert_eq!(schema["components"]["schemas"]["grant.Target"]["properties"]["accountId"]["$ref"],"#/components/schemas/account.Id");
            assert_eq!(schema["components"]["schemas"]["access.Entry"]["enum"],serde_json::json!(["domain","institution"]));
            stop.send(()).expect("stop"); let (application,ended)=task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);
            assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no supplier calls after registration through drain");
            observer.terminate().await.expect("observer ended"); sms.stop().await;
        }).await.expect("HTTP admission deadline");
    });
}

#[test]
fn workforce_state_history_http_preserves_fixed_facts_and_input_boundaries() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(50), async {
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
            let request = |method: reqwest::Method, path: &str, actor: &(String, String)| {
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.167.{address}"))
            };
            for (actor, entry, mobile, kind) in [(&institution,"institution","13800138000","full"),(&member,"domain","13900000000","default")] {
                let response = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", actor).json(&serde_json::json!({"entry":entry,"mobile":mobile})).send().await.expect("admission candidate");
                assert_eq!(response.status(), 200);
                let candidate = response.json::<serde_json::Value>().await.expect("confirmation");
                let role = candidate["roles"].as_array().expect("choices").iter().find(|role| role["kind"] == kind).expect("selected role");
                let response = request(reqwest::Method::POST, "/api/v1/workforce/grants", actor).json(&serde_json::json!({"entry":entry,"target":{
                    "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                    "mobile":mobile,"assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
                },"role":{"id":role["id"],"changeId":role["changeId"]}})).send().await.expect("admission");
                assert_eq!(response.status(), 201);
            }
            let candidate = request(reqwest::Method::POST, "/api/v1/workforce/suspensions/candidates", &member).json(&serde_json::json!({"mobile":"13900000000"})).send().await.expect("pause candidate").json::<serde_json::Value>().await.expect("confirmation");
            let grant = candidate["grantId"].as_str().expect("Grant ID");
            let body = serde_json::json!({"target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13900000000"}});
            for _ in 0..2 {
                let response = request(reqwest::Method::POST, &format!("/api/v1/workforce/grants/{grant}/suspensions"), &member).json(&body).send().await.expect("suspension");
                assert_eq!(response.status(), 201);
                let pause = response.json::<serde_json::Value>().await.expect("suspension fact");
                let id = pause["suspension"]["id"].as_str().expect("suspension ID");
                let response = request(reqwest::Method::POST, &format!("/api/v1/workforce/suspensions/{id}/resumptions"), &member).json(&body).send().await.expect("resumption");
                assert_eq!(response.status(), 201);
            }
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            for (table, field, at, item, operation_id) in [
                ("suspensions","suspended_at","suspendedAt","Suspension","listWorkforceSuspensions"),
                ("resumptions","resumed_at","resumedAt","Resumption","listWorkforceResumptions"),
            ] {
                let path = format!("/api/v1/workforce/{table}");
                let (join, grant_column) = if table == "suspensions" { ("","fact.grant_id") } else { ("JOIN access.suspensions suspension ON suspension.id=fact.id","suspension.grant_id") };
                let rows = observer.client().query(&format!("SELECT fact.id,{grant_column} AS grant_id,fact.{field} AS happened_at,fact.sequence,fact.operation_id,subject.person_id,subject.id AS revision_id,operator.person_id AS operator_person,operator.id AS operator_id FROM access.{table} fact {join} JOIN identity.revisions subject ON subject.id=fact.revision_id JOIN identity.revisions operator ON operator.id=fact.operator_id ORDER BY fact.{field} DESC,fact.id DESC"), &[]).await.expect("independent persisted facts before reading");
                assert_eq!(rows.len(), 2);
                let expected = rows.iter().map(|row| serde_json::json!({
                    "id":row.get::<_,Uuid>("id"),"grantId":row.get::<_,Uuid>("grant_id"),at:row.get::<_,chrono::DateTime<chrono::Utc>>("happened_at"),
                    "sequence":row.get::<_,i64>("sequence"),"operationId":row.get::<_,Uuid>("operation_id"),
                    "subject":{"personId":row.get::<_,Uuid>("person_id"),"revisionId":row.get::<_,Uuid>("revision_id"),"name":"F*"},
                    "operator":{"personId":row.get::<_,Uuid>("operator_person"),"revisionId":row.get::<_,Uuid>("operator_id"),"name":"F*"}
                })).collect::<Vec<_>>();
                let person: Uuid = rows[0].get("person_id");
                let operation: Uuid = rows[0].get("operation_id");
                for (query,count,values) in [
                    (String::new(),2,expected.clone()),
                    ("?size=100".into(),2,expected.clone()),
                    ("?page=1&size=1".into(),2,vec![expected[0].clone()]),
                    ("?page=2&size=1".into(),2,vec![expected[1].clone()]),
                    ("?page=3&size=1".into(),2,vec![]),
                    (format!("?personId={person}"),2,expected.clone()),
                    (format!("?grantId={grant}"),2,expected.clone()),
                    (format!("?operationId={operation}"),1,vec![expected[0].clone()]),
                    (format!("?personId={person}&grantId={grant}&operationId={operation}"),1,vec![expected[0].clone()]),
                    (format!("?grantId={}&operationId={operation}",Uuid::new_v4()),0,vec![]),
                ] {
                    let response = request(reqwest::Method::GET,&format!("{path}{query}"),&member).send().await.expect("history page");
                    assert_eq!(response.status(),200);
                    assert_eq!(response.headers()["cache-control"],"no-store");
                    assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("fixed masked shape"),serde_json::json!({"code":"ok","count":count,table:values}));
                }
                for query in ["page=0","page=-1","size=0","size=101","size=1.5","page=1&page=2","size=1&size=2","page=9223372036854775807&size=2","entry=domain","unknown=x","personId=invalid","grantId=","grantId=invalid","operationId=invalid","grantId=00000000-0000-4000-a000-000000000001&grantId=00000000-0000-4000-a000-000000000002"] {
                    let response=request(reqwest::Method::GET,&format!("{path}?{query}"),&member).send().await.expect("invalid parameter");
                    assert_eq!(response.status(),400,"{path}?{query}");
                    assert_eq!(response.json::<serde_json::Value>().await.expect("input failure"),serde_json::json!({"code":"invalid_input"}));
                }
                for (body,status) in [("{}".to_owned(),400),("x".repeat(8193),413)] {
                    let response=request(reqwest::Method::GET,&path,&member).body(body).send().await.expect("unexpected body");
                    assert_eq!(response.status(),status);
                }
                for (actor,query,status) in [(&member,"",200),(&institution,"",403),(&member,"?page=0",400)] {
                    let response=request(reqwest::Method::HEAD,&format!("{path}{query}"),actor).send().await.expect("HEAD");
                    assert_eq!(response.status(),status); assert_eq!(response.headers()["cache-control"],"no-store");
                    assert!(!response.headers().contains_key("set-cookie")); assert!(response.bytes().await.expect("HEAD body").is_empty());
                }
                let response=request(reqwest::Method::GET,&path,&institution).send().await.expect("institution only");
                assert_eq!(response.status(),403); assert_eq!(response.json::<serde_json::Value>().await.expect("denied"),serde_json::json!({"code":"permission_denied"}));
                assert_eq!(request(reqwest::Method::GET,&path,&member).header("origin","https://wrong.invalid").send().await.expect("wrong origin").status(),403);
                assert_eq!(request(reqwest::Method::GET,&path,&member).header("sec-fetch-site","cross-site").send().await.expect("cross-site").status(),403);
                assert_eq!(client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&member.0).header("cookie",&member.1).header("sec-fetch-site","same-origin").header("x-real-ip","203.0.168.250").send().await.expect("conditional origin").status(),200);
                assert_eq!(client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("x-real-ip","203.0.168.251").send().await.expect("missing session").status(),401);
                let owner=connections(&server,&name,"postgres").recover().await.expect("isolated query failure");
                owner.client().batch_execute(&format!("REVOKE SELECT ON access.{table} FROM clerk")).await.expect("revoke test privilege");
                let response=request(reqwest::Method::GET,&path,&member).send().await.expect("query failure");
                assert_eq!(response.status(),503); assert_eq!(response.json::<serde_json::Value>().await.expect("no false empty page"),serde_json::json!({"code":"dependency_unavailable"}));
                owner.client().batch_execute(&format!("GRANT SELECT ON access.{table} TO clerk")).await.expect("restore test privilege");
                assert_eq!(request(reqwest::Method::GET,&path,&member).send().await.expect("fresh read").status(),200);
                owner.terminate().await.expect("owner ended");
                let route=&document["paths"][&path]["get"];
                let schema=if table=="suspensions" {"grant.suspension"}else{"grant.resumption"};
                assert_eq!(route["operationId"],operation_id);
                assert_eq!(document["paths"][&path]["head"]["operationId"],format!("{operation_id}Head"));
                assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],format!("#/components/schemas/{schema}.List"));
                assert_eq!(document["components"]["schemas"][format!("{schema}.List")]["properties"][table]["items"]["$ref"],format!("#/components/schemas/{schema}.{item}"));
                let properties=document["components"]["schemas"][format!("{schema}.{item}")]["properties"].as_object().expect("fact schema");
                assert_eq!(properties.len(),7);
                for name in ["subject","operator"] { assert_eq!(properties[name]["$ref"],"#/components/schemas/person.revision.Identity"); }
                let parameters=route["parameters"].as_array().expect("parameters");
                assert!(parameters.iter().all(|parameter| parameter["name"]!="Origin" && parameter["name"]!="entry"));
                for (field,owner) in [("personId","person.Id"),("grantId","grant.Id"),("operationId","operation.Id")] {
                    let parameter=parameters.iter().find(|parameter| parameter["name"]==field).expect("typed filter");
                    assert_eq!(parameter["required"],false); assert_eq!(parameter["schema"]["$ref"],format!("#/components/schemas/{owner}"));
                }
                assert!(document["paths"][&path]["head"]["responses"]["200"].get("content").is_none());
            }
            let mut total=0;
            for path in ["/api/v1/workforce/suspensions","/api/v1/workforce/resumptions"] {
                total+=request(reqwest::Method::GET,path,&member).send().await.expect("history count").json::<serde_json::Value>().await.expect("history")["count"].as_i64().expect("count");
            }
            assert_eq!(total,4,"no history writes");
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let(application,ended)=task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);
            assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded state history HTTP acceptance");
    });
}

#[test]
fn workforce_termination_history_http_unites_ends_and_preserves_input_boundaries() {
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
            let request = |method: reqwest::Method, path: &str, actor: &(String, String)| {
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.169.{address}"))
            };
            let path = "/api/v1/workforce/terminations";
            let response = request(reqwest::Method::GET, path, &institution).send().await.expect("institution only");
            assert_eq!(response.status(),403);
            assert_eq!(response.json::<serde_json::Value>().await.expect("denial"),serde_json::json!({"code":"permission_denied"}));
            for mode in ["reader", "self", "manager", "cancellation"] {
                let (actor, entry, mobile, kind) = if mode=="reader" {(&institution,"institution","13800138000","full")} else {(&member,"domain","13900000000","default")};
                let response = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", actor).json(&serde_json::json!({"entry":entry,"mobile":mobile})).send().await.expect("candidate");
                assert_eq!(response.status(),200);
                let candidate = response.json::<serde_json::Value>().await.expect("candidate JSON");
                let role = candidate["roles"].as_array().expect("choices").iter().find(|role|role["kind"]==kind).expect("role");
                let response = request(reqwest::Method::POST, "/api/v1/workforce/grants", actor).json(&serde_json::json!({"entry":entry,"target":{
                    "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":mobile,
                    "assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
                },"role":{"id":role["id"],"changeId":role["changeId"]}})).send().await.expect("admission");
                assert_eq!(response.status(),201);
                let grant=response.json::<serde_json::Value>().await.expect("grant");
                if mode=="reader" { continue; }
                let grant=grant["grant"]["id"].as_str().expect("ID");
                assert_eq!(request(reqwest::Method::HEAD,path,&institution).send().await.expect("institution HEAD").status(),403);
                let response=match mode {
                    "self"=>request(reqwest::Method::POST,&format!("/api/v1/workforce/grants/{grant}/terminations"),&institution).send().await.expect("self exit"),
                    "manager"=>request(reqwest::Method::POST,&format!("/api/v1/workforce/grants/{grant}/terminations"),&member).json(&serde_json::json!({"target":{
                        "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":mobile
                    }})).send().await.expect("manager end"),
                    "cancellation"=>request(reqwest::Method::POST,"/api/v1/accounts/current/cancellations",&institution).json(&serde_json::json!({
                        "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"]
                    })).send().await.expect("self cancellation"),
                    _=>unreachable!(),
                };
                assert_eq!(response.status(),if mode=="cancellation" {200}else{201});
            }
            let observer=connections(&server,&name,"clerk").recover().await.expect("observer");
            let rows=observer.client().query("SELECT termination.id,termination.person_id,termination.revision_id,termination.operator_id,operator.person_id AS operator_person,termination.terminated_at,termination.sequence,termination.operation_id,termination.mode FROM access.terminations termination JOIN identity.revisions operator ON operator.id=termination.operator_id ORDER BY terminated_at DESC,termination.id DESC",&[]).await.expect("independent permanent termination records");
            let expected=rows.iter().map(|row|serde_json::json!({
                "id":row.get::<_,Uuid>("id"),"mode":row.get::<_,String>("mode"),"terminatedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("terminated_at"),"sequence":row.get::<_,i64>("sequence"),"operationId":row.get::<_,Uuid>("operation_id"),
                "subject":{"personId":row.get::<_,Uuid>("person_id"),"revisionId":row.get::<_,Uuid>("revision_id"),"name":"F*"},
                "operator":{"personId":row.get::<_,Uuid>("operator_person"),"revisionId":row.get::<_,Uuid>("operator_id"),"name":"F*"}
            })).collect::<Vec<_>>();
            assert_eq!(expected.len(),3);
            assert_eq!(expected.iter().map(|item|item["mode"].as_str().expect("mode")).collect::<Vec<_>>(),["cancellation","manager","self"]);
            let person=rows[0].get::<_,Uuid>("person_id"); let grant=rows[0].get::<_,Uuid>("id"); let operation=rows[0].get::<_,Uuid>("operation_id");
            let admission:Uuid=observer.client().query_one("SELECT operation_id FROM access.grants WHERE id=$1",&[&grant]).await.expect("admission operation").get(0);
            for (query,count,values) in [
                (String::new(),3,expected.clone()),("?size=100".into(),3,expected.clone()),
                ("?page=1&size=1".into(),3,vec![expected[0].clone()]),("?page=2&size=1".into(),3,vec![expected[1].clone()]),("?page=3&size=1".into(),3,vec![expected[2].clone()]),("?page=4&size=1".into(),3,vec![]),
                (format!("?personId={person}"),3,expected.clone()),(format!("?grantId={grant}"),1,vec![expected[0].clone()]),(format!("?operationId={operation}"),1,vec![expected[0].clone()]),
                (format!("?personId={person}&grantId={grant}&operationId={operation}"),1,vec![expected[0].clone()]),(format!("?operationId={admission}"),0,vec![]),
            ] {
                let response=request(reqwest::Method::GET,&format!("{path}{query}"),&member).send().await.expect("page");
                assert_eq!(response.status(),200); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("masked fixed shape"),serde_json::json!({"code":"ok","count":count,"terminations":values}));
            }
            for query in ["page=0","page=-1","size=0","size=101","size=1.5","page=1&page=2","size=1&size=2","page=9223372036854775807&size=2","entry=domain","unknown=x","personId=invalid","grantId=","grantId=invalid","operationId=invalid","grantId=00000000-0000-4000-a000-000000000001&grantId=00000000-0000-4000-a000-000000000002"] {
                let response=request(reqwest::Method::GET,&format!("{path}?{query}"),&member).send().await.expect("invalid query");
                assert_eq!(response.status(),400,"{query}"); assert_eq!(response.json::<serde_json::Value>().await.expect("failure"),serde_json::json!({"code":"invalid_input"}));
            }
            for (body,status) in [("{}".to_owned(),400),("x".repeat(8193),413)] { assert_eq!(request(reqwest::Method::GET,path,&member).body(body).send().await.expect("body rejection").status(),status); }
            for (query,status) in [("",200),("?page=0",400)] {
                let response=request(reqwest::Method::HEAD,&format!("{path}{query}"),&member).send().await.expect("HEAD");
                assert_eq!(response.status(),status); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie")); assert!(response.bytes().await.expect("empty HEAD").is_empty());
            }
            assert_eq!(request(reqwest::Method::GET,path,&member).header("origin","https://wrong.invalid").send().await.expect("wrong origin").status(),403);
            assert_eq!(request(reqwest::Method::GET,path,&member).header("sec-fetch-site","cross-site").send().await.expect("cross-site").status(),403);
            assert_eq!(client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&member.0).header("cookie",&member.1).header("sec-fetch-site","same-origin").header("x-real-ip","203.0.170.250").send().await.expect("same-origin without Origin").status(),200);
            assert_eq!(client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("x-real-ip","203.0.170.251").send().await.expect("no session").status(),401);
            assert_eq!(request(reqwest::Method::GET,path,&institution).send().await.expect("cancelled reader").status(),401);
            let document=client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let route=&document["paths"][path]["get"];
            assert_eq!(route["operationId"],"listWorkforceTerminations");
            assert_eq!(document["paths"][path]["head"]["operationId"],"listWorkforceTerminationsHead");
            assert!(document["paths"][path]["head"]["responses"]["200"].get("content").is_none());
            assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/grant.termination.List");
            assert_eq!(document["components"]["schemas"]["grant.termination.List"]["properties"]["terminations"]["items"]["$ref"],"#/components/schemas/termination.Termination");
            let properties=document["components"]["schemas"]["termination.Termination"]["properties"].as_object().expect("fact schema");
            assert_eq!(properties.len(),7);
            for field in ["subject","operator"] { assert_eq!(properties[field]["$ref"],"#/components/schemas/person.revision.Identity"); }
            let parameters=route["parameters"].as_array().expect("parameters");
            assert!(parameters.iter().all(|parameter| parameter["name"]!="Origin" && parameter["name"]!="entry"));
            for (field,owner) in [("personId","person.Id"),("grantId","grant.Id"),("operationId","operation.Id")] {
                let parameter=parameters.iter().find(|parameter|parameter["name"]==field).expect("filter");
                assert_eq!(parameter["required"],false); assert_eq!(parameter["schema"]["$ref"],format!("#/components/schemas/{owner}"));
            }
            assert_eq!(observer.client().query_one("SELECT count(*) FROM access.terminations",&[]).await.expect("every end retains its own record").get::<_,i64>(0),3);
            assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.terminations termination JOIN identity.cancellations cause ON cause.id=termination.cancellation_id WHERE termination.id=$1 AND termination.mode='cancellation' AND cause.person_id=termination.person_id AND cause.revision_id=termination.revision_id AND cause.operator_id=termination.operator_id AND cause.cancelled_at=termination.terminated_at AND cause.operation_id=termination.operation_id AND cause.transaction_id=termination.transaction_id)",&[&grant]).await.expect("cancellation termination retains its exact source").get::<_,bool>(0));
            let owner=connections(&server,&name,"postgres").recover().await.expect("isolated owner");
            owner.client().batch_execute("REVOKE SELECT ON access.terminations FROM clerk").await.expect("query failure");
            let response=request(reqwest::Method::GET,path,&member).send().await.expect("failed read");
            assert_eq!(response.status(),503); assert_eq!(response.json::<serde_json::Value>().await.expect("error"),serde_json::json!({"code":"dependency_unavailable"}));
            owner.client().batch_execute("GRANT SELECT ON access.terminations TO clerk").await.expect("restore access");
            assert_eq!(request(reqwest::Method::GET,path,&member).send().await.expect("fresh page").status(),200);
            owner.terminate().await.expect("owner ended"); observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let(application,ended)=task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);
            assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded termination history HTTP acceptance");
    });
}

#[test]
fn workforce_grant_history_http_preserves_fixed_facts_and_input_boundaries() {
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
            let request = |method: reqwest::Method, path: &str, actor: &(String, String)| {
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.166.{address}"))
            };
            let path = "/api/v1/workforce/grants";
            for actor in [&manager, &member] {
                let response = request(reqwest::Method::GET, path, actor).send().await.expect("no domain authority");
                assert_eq!(response.status(), 403);
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe denial"), serde_json::json!({"code":"permission_denied"}));
            }
            let response = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", &manager)
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("real candidate");
            assert_eq!(response.status(), 200);
            let candidate = response.json::<serde_json::Value>().await.expect("confirmation");
            let full = candidate["roles"].as_array().expect("choices").iter().find(|role| role["kind"] == "full").expect("full role");
            let body = serde_json::json!({"entry":"institution","target":{
                "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
            },"role":{"id":full["id"],"changeId":full["changeId"]}});
            let response = request(reqwest::Method::POST, path, &manager).json(&body).send().await.expect("real admission");
            assert_eq!(response.status(), 201);
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
            let row = observer.client().query_one("SELECT admission.id, admission.granted_at, admission.sequence, admission.operation_id, subject.person_id, subject.id AS revision_id, operator.person_id AS operator_person, operator.id AS operator_id FROM access.grants admission JOIN identity.revisions subject ON subject.id=admission.revision_id JOIN identity.revisions operator ON operator.id=admission.operator_id", &[]).await.expect("independent fixed facts before history reads");
            let id: Uuid = row.get("id"); let person: Uuid = row.get("person_id"); let operation: Uuid = row.get("operation_id");
            let expected = serde_json::json!({"id":id,"grantedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("granted_at"),"sequence":row.get::<_,i64>("sequence"),"operationId":operation,"status":"active",
                "subject":{"personId":person,"revisionId":row.get::<_,Uuid>("revision_id"),"name":"F*"},
                "operator":{"personId":row.get::<_,Uuid>("operator_person"),"revisionId":row.get::<_,Uuid>("operator_id"),"name":"F*"}});
            for (query, count, grants) in [
                (String::new(), 1, vec![expected.clone()]),
                ("?page=2&size=1".into(), 1, vec![]),
                ("?size=100".into(), 1, vec![expected.clone()]),
                (format!("?personId={person}&grantId={id}&operationId={operation}"), 1, vec![expected.clone()]),
                (format!("?grantId={}", Uuid::new_v4()), 0, vec![]),
            ] {
                let response = request(reqwest::Method::GET, &format!("{path}{query}"), &member).send().await.expect("history page");
                assert_eq!(response.status(), 200);
                assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("minimal fixed facts"), serde_json::json!({"code":"ok","count":count,"grants":grants}));
            }
            for query in ["page=0", "size=0", "size=101", "page=1&page=2", "page=9223372036854775807&size=2", "entry=domain", "personId=invalid", "grantId=", "grantId=invalid", "operationId=invalid", "grantId=00000000-0000-4000-a000-000000000001&grantId=00000000-0000-4000-a000-000000000002"] {
                let response = request(reqwest::Method::GET, &format!("{path}?{query}"), &member).send().await.expect("invalid filter");
                assert_eq!(response.status(), 400, "{query}");
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe input failure"), serde_json::json!({"code":"invalid_input"}));
            }
            for (body, status) in [("{}".to_owned(), 400), ("x".repeat(8193), 413)] {
                let response = request(reqwest::Method::GET, path, &member).body(body).send().await.expect("unexpected body");
                assert_eq!(response.status(), status);
                assert_eq!(response.json::<serde_json::Value>().await.expect("body failure"), serde_json::json!({"code":"invalid_input"}));
            }
            let response = request(reqwest::Method::HEAD, path, &member).send().await.expect("HEAD history");
            assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie")); assert!(response.bytes().await.expect("HEAD bytes").is_empty());
            assert_eq!(request(reqwest::Method::HEAD, path, &manager).send().await.expect("HEAD denied").status(), 403);
            assert_eq!(request(reqwest::Method::GET, path, &member).header("origin", "https://wrong.invalid").send().await.expect("wrong Origin").status(), 403);
            assert_eq!(request(reqwest::Method::GET, path, &member).header("sec-fetch-site", "cross-site").send().await.expect("cross-site denied").status(), 403);
            let response = client.get(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &member.0).header("cookie", &member.1)
                .header("sec-fetch-site", "same-origin").header("x-real-ip", "203.0.166.250").send().await.expect("same-origin metadata without Origin");
            assert_eq!(response.status(), 200);
            assert_eq!(client.get(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string()).header("x-real-ip", "203.0.166.251").send().await.expect("missing identity").status(), 401);
            let owner = connections(&server, &name, "postgres").recover().await.expect("isolated query failure");
            owner.client().batch_execute("REVOKE SELECT ON access.grants FROM clerk").await.expect("temporary test permission failure");
            let response = request(reqwest::Method::GET, path, &member).send().await.expect("query failure");
            assert_eq!(response.status(), 503); assert_eq!(response.json::<serde_json::Value>().await.expect("no false empty history"), serde_json::json!({"code":"dependency_unavailable"}));
            owner.client().batch_execute("GRANT SELECT ON access.grants TO clerk").await.expect("restore test permission");
            assert_eq!(request(reqwest::Method::GET, path, &member).send().await.expect("fresh read").status(), 200);
            assert_eq!(request(reqwest::Method::GET, path, &member).send().await.expect("grant count").json::<serde_json::Value>().await.expect("history")["count"], 1, "reads preserve permanent facts");
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let route = &document["paths"][path]["get"];
            assert_eq!(route["operationId"], "listWorkforceGrants");
            assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/grant.List");
            let parameters = route["parameters"].as_array().expect("parameters");
            assert!(parameters.iter().all(|parameter| parameter["name"] != "Origin" && parameter["name"] != "entry"));
            for (field, owner) in [("personId", "person.Id"), ("grantId", "grant.Id"), ("operationId", "operation.Id")] {
                let parameter = parameters.iter().find(|parameter| parameter["name"] == field).expect("typed filter");
                assert_eq!(parameter["required"], false); assert_eq!(parameter["schema"]["$ref"], format!("#/components/schemas/{owner}"));
                assert_eq!(document["components"]["schemas"][owner]["format"], "uuid");
            }
            assert!(document["paths"][path]["head"]["responses"]["200"].get("content").is_none());
            assert!(document["paths"][path].get("post").is_some());
            owner.terminate().await.expect("owner ended"); observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application, ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
            assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded Grant history HTTP acceptance");
    });
}

#[test]
fn workforce_grant_read_http_returns_history_and_first_assignment() {
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
            let request = |method: reqwest::Method, path: &str, actor: &(String, String)| {
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.167.{address}"))
            };
            let response = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", &manager)
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate");
            assert_eq!(response.status(), 200);
            let candidate = response.json::<serde_json::Value>().await.expect("confirmation");
            let full = candidate["roles"].as_array().expect("choices").iter().find(|role| role["kind"] == "full").expect("full role");
            let response = request(reqwest::Method::POST, "/api/v1/workforce/grants", &manager)
                .json(&serde_json::json!({"entry":"institution","target":{
                    "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                    "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
                },"role":{"id":full["id"],"changeId":full["changeId"]}})).send().await.expect("admission");
            assert_eq!(response.status(), 201);
            let original = response.json::<serde_json::Value>().await.expect("original result");
            let id = original["grant"]["id"].as_str().expect("Grant ID");
            let path = format!("/api/v1/workforce/grants/{id}");
            let response = request(reqwest::Method::GET, &format!("/api/v1/workforce/grants?grantId={id}"), &member).send().await.expect("list item");
            assert_eq!(response.status(), 200);
            let list = response.json::<serde_json::Value>().await.expect("history");
            let response = request(reqwest::Method::GET, &path, &member).send().await.expect("single read");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            let value = response.json::<serde_json::Value>().await.expect("complete result");
            assert_eq!(value, serde_json::json!({"code":"ok","grant":list["grants"][0],"assignment":original["assignment"]}));
            assert_eq!(value, original);
            assert_eq!(value["grant"].as_object().expect("Grant projection").len(), 7);
            assert_eq!(value["assignment"]["role"], *full);
            for record in ["grant", "assignment"] {
                for identity in ["subject", "operator"] {
                    assert_eq!(value[record][identity].as_object().expect("masked identity").len(), 3);
                    assert_eq!(value[record][identity]["name"], "F*");
                }
            }
            let unknown = format!("/api/v1/workforce/grants/{}", Uuid::new_v4());
            for (response, status, code) in [
                (request(reqwest::Method::GET, &unknown, &member).send().await.expect("unknown"), 404, "target_unavailable"),
                (request(reqwest::Method::GET, &path, &manager).send().await.expect("institution-only denied"), 403, "permission_denied"),
                (request(reqwest::Method::GET, &unknown, &manager).send().await.expect("permission before absence"), 403, "permission_denied"),
                (request(reqwest::Method::GET, "/api/v1/workforce/grants/invalid", &member).send().await.expect("invalid path"), 400, "invalid_input"),
                (request(reqwest::Method::GET, &format!("{path}?page=1"), &member).send().await.expect("query rejected"), 400, "invalid_input"),
                (request(reqwest::Method::GET, &path, &member).body("{}").send().await.expect("body rejected"), 400, "invalid_input"),
                (request(reqwest::Method::GET, &path, &member).body("x".repeat(8193)).send().await.expect("body limit"), 413, "invalid_input"),
            ] {
                assert_eq!(response.status(), status);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure"), serde_json::json!({"code":code}));
            }
            for (target, status) in [("/api/v1/workforce/grants/invalid", 400), (path.as_str(), 401)] {
                let response = client.get(format!("{base}{target}")).header("operation-id", Uuid::new_v4().to_string()).header("x-real-ip", "203.0.167.250").send().await.expect("path before authentication");
                assert_eq!(response.status(), status);
            }
            for (target, status) in [(&path, 200), (&unknown, 404)] {
                let response = request(reqwest::Method::HEAD, target, &member).send().await.expect("HEAD");
                assert_eq!(response.status(), status); assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie")); assert!(response.bytes().await.expect("HEAD bytes").is_empty());
            }
            assert_eq!(request(reqwest::Method::GET, &path, &member).header("origin", "https://wrong.invalid").send().await.expect("wrong origin").status(), 403);
            assert_eq!(request(reqwest::Method::GET, &path, &member).header("sec-fetch-site", "cross-site").send().await.expect("cross-site").status(), 403);
            assert_eq!(client.get(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &member.0).header("cookie", &member.1)
                .header("sec-fetch-site", "same-origin").header("x-real-ip", "203.0.167.251").send().await.expect("read without Origin").status(), 200);
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let route = &document["paths"]["/api/v1/workforce/grants/{id}"]["get"];
            assert_eq!(route["operationId"], "readWorkforceGrant");
            assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/grant.Receipt");
            assert_eq!(route["responses"]["200"]["content"], document["paths"]["/api/v1/workforce/grants"]["post"]["responses"]["201"]["content"]);
            assert!(route.get("requestBody").is_none());
            let parameters = route["parameters"].as_array().expect("parameters");
            assert_eq!(parameters.len(), 3);
            let parameter = parameters.iter().find(|parameter| parameter["in"] == "path").expect("path ID");
            assert_eq!(parameter["schema"]["$ref"], "#/components/schemas/grant.Id"); assert_eq!(parameter["required"], true);
            let receipt = &document["components"]["schemas"]["grant.Receipt"];
            assert_eq!(receipt["required"], serde_json::json!(["code","grant","assignment"]));
            assert_eq!(receipt["properties"]["grant"]["$ref"], "#/components/schemas/grant.Grant");
            assert_eq!(receipt["properties"]["assignment"]["$ref"], "#/components/schemas/assignment.Assignment");
            let head = &document["paths"]["/api/v1/workforce/grants/{id}"]["head"];
            assert_eq!(head["operationId"], "readWorkforceGrantHead"); assert!(head["responses"]["200"].get("content").is_none());
            stop.send(()).expect("stop"); let (application, ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
            assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded single Grant HTTP acceptance");
    });
}

#[test]
fn current_workforce_reads_preserve_identity_boundaries_and_business_facts() {
    const OPERATION: &str = "00000000-0000-4000-9000-000000000104";
    if std::env::var_os("NUCLEUS_TEST_CURRENT_WORKFORCE").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            for kind in ["grant", "role"] {
                let expectations = tempfile::NamedTempFile::new().expect("private expectations");
                let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", "grant::current_workforce_reads_preserve_identity_boundaries_and_business_facts", "--nocapture"])
                    .env("NUCLEUS_TEST_CURRENT_WORKFORCE", kind).env("NUCLEUS_TEST_CURRENT_WORKFORCE_EXPECTATIONS", expectations.path())
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true)
                    .spawn().expect("isolated diagnostics child");
                let output = timeout(Duration::from_secs(50), child.wait_with_output()).await.expect("child deadline").expect("child completed");
                let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
                assert!(output.status.success(), "{text}");
                let events: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).expect("JSON event")).collect();
                let failures: Vec<_> = events.iter().filter(|event| event["level"] == "ERROR").collect();
                assert_eq!(failures.len(), 1, "one final query failure through later reads and drain: {text}");
                let event = failures[0];
                assert_eq!(event["fields"]["stage"], if kind == "grant" { "grant_read" } else { "workforce_role_read" }); assert_eq!(event["fields"]["kind"], "unavailable");
                assert!(event["fields"]["error"].as_str().expect("safe query cause").contains("Query("));
                let request = event["spans"].as_array().expect("request spans").iter().find(|span| span["route"] == format!("/api/v1/workforce/current/{kind}")).expect("read request");
                assert_eq!(request["method"], "GET"); assert_eq!(request["operation_id"], OPERATION);
                let request_id = request["request_id"].as_str().expect("request ID"); Uuid::parse_str(request_id).expect("request UUID");
                let responses: Vec<_> = events.iter().filter(|event| event["fields"]["stage"] == "http_response" && event["spans"].as_array().is_some_and(|spans| spans.iter().any(|span| span["request_id"] == request_id))).collect();
                assert_eq!(responses.len(), 1); assert_eq!(responses[0]["fields"]["status"], 503);
                let tokens: Vec<String> = serde_json::from_slice(&std::fs::read(expectations.path()).expect("private expected tokens")).expect("tokens");
                for secret in tokens.iter().map(String::as_str).chain(["13800138000", "13900000000", "Fixture Registered", "Fixture Operator", "00000020000229007X", "000000200002290010", "__Host-nucleus-session", "203.0.137.", fixture::PASSWORD, "SELECT ", "INSERT ", "UPDATE "]) {
                    assert!(!events.iter().any(|event| contains_text(event, secret)), "private material in current Workforce diagnostics");
                }
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("production diagnostics");
    isolated_child(|| {
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
                let (base, stop, task) = running(application).await;
                let manager = issued_session_for(&base, &mut sms, "13900000000").await;
                let ordinary = issued_session_for(&base, &mut sms, "13800138000").await;
                let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
                let kind = std::env::var("NUCLEUS_TEST_CURRENT_WORKFORCE").expect("selected read");
                let path = format!("/api/v1/workforce/current/{kind}");
                let absent = if kind == "grant" { "grant_absent" } else { "role_absent" };
                let read = |method, identity: &(String, String), source: u8| client.request(method, format!("{base}{path}"))
                    .header("operation-id", Uuid::new_v4().to_string()).header("session-id", &identity.0).header("cookie", &identity.1)
                    .header("x-real-ip", format!("203.0.137.{source}"));
                let post = |path: &str| client.post(format!("{base}{path}"))
                    .header("operation-id", Uuid::new_v4().to_string()).header("session-id", &manager.0).header("cookie", &manager.1)
                    .header("origin", "https://example.com").header("x-real-ip", "203.0.137.250");
                let response = read(reqwest::Method::GET, &ordinary, 1).header("sec-fetch-site", "same-origin").send().await.expect("never admitted");
                assert_eq!(response.status(), 404);
                assert_eq!(response.json::<serde_json::Value>().await.expect("absence"), serde_json::json!({"code":absent}));
                let response = post("/api/v1/workforce/grants/candidates").json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate");
                assert_eq!(response.status(), 200);
                let candidate = response.json::<serde_json::Value>().await.expect("candidate");
                let selected_kind = if kind == "role" { "full" } else { "default" };
                let role = candidate["roles"].as_array().expect("choices").iter().find(|role| role["kind"] == selected_kind).expect("selected system role");
                let body = serde_json::json!({"entry":"institution","target":{
                    "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                    "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":null,"grant":null
                },"role":{"id":role["id"],"changeId":role["changeId"]}});
                assert_eq!(post("/api/v1/workforce/grants").json(&body).send().await.expect("ordinary member admitted").status(), 201);
                let observer = connections(&server, &name, "clerk").recover().await.expect("independent runtime observer");
                let fact = observer.client().query_one("SELECT admission.id,admission.person_id,admission.revision_id,admission.operator_id,operator.person_id AS operator,admission.granted_at,admission.operation_id FROM access.grants admission JOIN identity.revisions operator ON operator.id=admission.operator_id", &[]).await.expect("actual permanent fact");
                let expected = if kind == "grant" { serde_json::json!({"code":"ok","grant":{
                    "id":fact.get::<_,Uuid>("id"),"subject":{"personId":fact.get::<_,Uuid>("person_id"),"revisionId":fact.get::<_,Uuid>("revision_id"),"name":"F*"},
                    "operator":{"personId":fact.get::<_,Uuid>("operator"),"revisionId":fact.get::<_,Uuid>("operator_id"),"name":"F*"},
                    "grantedAt":fact.get::<_,chrono::DateTime<chrono::Utc>>("granted_at"),"operationId":fact.get::<_,Uuid>("operation_id"),"sequence":1,"status":"active"
                }}) } else {
                    let role = observer.client().query_one("SELECT role.id,role.change_id,ARRAY(SELECT id FROM access.capabilities WHERE scope='workforce' ORDER BY id COLLATE \"C\") AS permissions FROM access.grants member JOIN access.roles role ON role.id=member.role_id WHERE member.person_id=$1 AND member.termination IS NULL", &[&fact.get::<_,Uuid>("person_id")]).await.expect("independent full role definition");
                    serde_json::json!({"code":"ok","role":{"id":role.get::<_,Uuid>("id"),"kind":"full","name":"管理员","level":1000,"permissions":role.get::<_,Vec<String>>("permissions"),"changeId":role.get::<_,Uuid>("change_id")}})
                };
                for response in [
                    read(reqwest::Method::GET, &ordinary, 2).header("sec-fetch-site", "same-origin").send().await.expect("same-origin metadata"),
                    read(reqwest::Method::GET, &ordinary, 3).header("origin", "https://example.com").send().await.expect("explicit valid origin"),
                ] {
                    assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store");
                    assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("fixed masked projection"), expected);
                }
                for (method, identity, status) in [(reqwest::Method::HEAD, &ordinary, 200), (reqwest::Method::HEAD, &manager, 404)] {
                    let response = read(method, identity, 4).header("sec-fetch-site", "same-origin").send().await.expect("HEAD");
                    assert_eq!(response.status(), status); assert_eq!(response.headers()["cache-control"], "no-store");
                    assert!(!response.headers().contains_key("set-cookie")); assert!(response.bytes().await.expect("no HEAD body").is_empty());
                }
                for (builder, status, code) in [
                    (read(reqwest::Method::GET, &manager, 5).header("sec-fetch-site", "same-origin"), 404, absent),
                    (read(reqwest::Method::GET, &ordinary, 6), 403, "permission_denied"),
                    (read(reqwest::Method::GET, &ordinary, 7).header("origin", "https://wrong.invalid"), 403, "permission_denied"),
                    (read(reqwest::Method::GET, &ordinary, 8).header("origin", "https://example.com").header("sec-fetch-site", "cross-site"), 403, "permission_denied"),
                    (read(reqwest::Method::GET, &ordinary, 9).header("sec-fetch-site", "same-origin").query(&[("personId", fact.get::<_,Uuid>("operator").to_string())]), 400, "invalid_input"),
                    (read(reqwest::Method::GET, &ordinary, 10).header("sec-fetch-site", "same-origin").body("{}"), 400, "invalid_input"),
                    (read(reqwest::Method::GET, &ordinary, 11).header("sec-fetch-site", "same-origin").body(vec![b'x'; 8193]), 413, "invalid_input"),
                    (read(reqwest::Method::GET, &ordinary, 12).header("sec-fetch-site", "same-origin").header("operation-id", "invalid"), 400, "invalid_input"),
                    (read(reqwest::Method::GET, &ordinary, 13).header("sec-fetch-site", "same-origin").header("authorization", "Bearer invalid"), 400, "invalid_input"),
                ] {
                    let response = builder.send().await.expect("public read boundary"); assert_eq!(response.status(), status);
                    assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure"), serde_json::json!({"code":code}));
                }
                let response = client.get(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("x-real-ip","203.0.137.14").send().await.expect("no credentials");
                assert_eq!(response.status(), 401);
                let response = read(reqwest::Method::POST, &ordinary, 15).send().await.expect("unsupported method");
                assert_eq!(response.status(), 405);
                for method in ["GET", "HEAD"] { assert!(response.headers()["allow"].to_str().expect("allow").contains(method)); }
                let schema = client.get(format!("{base}/openapi/v1.json")).send().await.expect("schema").json::<serde_json::Value>().await.expect("OpenAPI");
                let route = &schema["paths"][&path];
                let (operation, receipt, model, id) = if kind == "grant" {
                    ("currentWorkforceGrant", "grant.Current", "grant.Grant", "grant.Id")
                } else { ("currentWorkforceRole", "role.Receipt", "role.Role", "role.Id") };
                assert_eq!(route["get"]["operationId"], operation);
                assert_eq!(route["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], format!("#/components/schemas/{receipt}"));
                assert_eq!(schema["components"]["schemas"][receipt]["properties"][&kind]["$ref"], format!("#/components/schemas/{model}"));
                assert_eq!(schema["components"]["schemas"][model]["properties"]["id"]["$ref"], format!("#/components/schemas/{id}"));
                for method in ["get", "head"] {
                    let parameters = route[method]["parameters"].as_array().expect("headers");
                    assert_eq!(parameters.len(), 2);
                    for name in ["Operation-Id", "Session-Id"] { assert_eq!(parameters.iter().find(|item| item["name"]==name).expect("required header")["required"], true); }
                    assert!(route[method].get("requestBody").is_none());
                    for status in ["200", "400", "401", "403", "404", "413", "429", "500", "503"] {
                        assert_eq!(route[method]["responses"][status]["headers"]["Cache-Control"]["description"], "no-store");
                        if method == "head" { assert!(route[method]["responses"][status].get("content").is_none()); }
                    }
                }
                let response = client.delete(format!("{base}/api/v1/sessions/current")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&ordinary.0).header("cookie",&ordinary.1).header("origin","https://example.com").header("x-real-ip","203.0.137.16").send().await.expect("real session revocation");
                assert_eq!(response.status(), 200);
                let response = read(reqwest::Method::GET, &ordinary, 17).header("sec-fetch-site", "same-origin").send().await.expect("original session no longer valid");
                assert_eq!(response.status(), 401); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("invalid identity"), serde_json::json!({"code":"authentication_invalid"}));
                let owner = connections(&server, &name, "postgres").recover().await.expect("isolated query failure injector");
                owner.client().batch_execute("REVOKE SELECT ON access.grants FROM clerk").await.expect("query failure after authentication");
                let response = client.get(format!("{base}{path}")).header("operation-id", OPERATION).header("session-id", &manager.0).header("cookie", &manager.1).header("sec-fetch-site", "same-origin").header("x-real-ip", "203.0.137.18").send().await.expect("actual failed query");
                assert_eq!(response.status(), 503); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe dependency failure"), serde_json::json!({"code":"dependency_unavailable"}));
                owner.client().batch_execute("GRANT SELECT ON access.grants TO clerk").await.expect("restore isolated permission");
                owner.terminate().await.expect("owner ended");
                assert_eq!(read(reqwest::Method::GET, &manager, 19).header("sec-fetch-site", "same-origin").send().await.expect("ordinary query failure kept service available").status(), 404);
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM access.grants)=1 AND (SELECT count(*) FROM access.assignments)=1 AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=1 AND (SELECT sequence AS person_sequence FROM identity.persons WHERE id=$1)=1", &[&fact.get::<_,Uuid>("person_id")]).await.expect("reads leave business facts unchanged").get::<_,bool>(0));
                stop.send(()).expect("stop"); let (application, ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
                assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err());
                observer.terminate().await.expect("observer ended"); sms.stop().await;
                let tokens = [manager.1, ordinary.1].map(|cookie| cookie.split_once('=').expect("actual cookie").1.to_owned());
                std::fs::write(std::env::var_os("NUCLEUS_TEST_CURRENT_WORKFORCE_EXPECTATIONS").expect("expectation path"), serde_json::to_vec(&tokens).expect("tokens")).expect("private expected tokens written");
            }).await.expect("self Workforce HTTP deadline");
        });
    });
}

#[test]
fn workforce_writes_report_unknown_and_committed_cleanup_outcomes_once() {
    use fixture::database_proxy::{Fault, Proxy};

    const OPERATION: &str = "00000000-0000-4000-9000-000000000001";
    if std::env::var_os("NUCLEUS_TEST_GRANT_DIAGNOSTICS").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            for surface in ["assignment", "admission", "termination", "suspension", "resumption"] {
            for case in ["unknown", "committed"] {
                let expected = tempfile::NamedTempFile::new().expect("private cookie expectations");
                let child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", "grant::workforce_writes_report_unknown_and_committed_cleanup_outcomes_once", "--nocapture"])
                    .env("NUCLEUS_TEST_GRANT_DIAGNOSTICS", expected.path()).env("NUCLEUS_TEST_GRANT_CASE", case).env("NUCLEUS_TEST_GRANT_SURFACE", surface)
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().expect("diagnostics child");
                let output = timeout(Duration::from_secs(45), child.wait_with_output()).await.expect("bounded child").expect("child output");
                let text = String::from_utf8(output.stderr).expect("JSON events");
                assert!(output.status.success(), "{text}");
                let events: Vec<serde_json::Value> = text.lines().map(|line| serde_json::from_str(line).expect("JSON event")).collect();
                let failures: Vec<_> = events.iter().filter(|event| event["level"] == "ERROR").collect();
                assert_eq!(failures.len(), 1, "one final Workforce failure through replay and drain: {text}");
                let event = failures[0];
                assert_eq!(event["fields"]["stage"], if case == "unknown" { "command" } else { "completion_recovery" });
                assert_eq!(event["fields"]["operation_id"], OPERATION);
                if case == "unknown" {
                    assert_eq!(event["fields"]["kind"], "unknown");
                    assert_eq!(event["target"], "domain::kernel::error");
                    let error=event["fields"]["error"].as_str().expect("original and recovery causes");
                    assert!(error.starts_with("Unknown("));
                    assert!(error.contains("stage: Commit") && error.contains("(Begin, Query(connection closed))"), "{error}");
                } else {
                    assert_eq!(event["target"], "domain::kernel::executor");
                    assert_eq!(event["fields"]["outcome"], "committed");
                    let error=event["fields"]["error"].as_str().expect("known commit retains the cleanup cause");
                    assert!(error.contains("Termination") && error.contains("Connection(database driver connection failed)"), "{error}");
                }
                let spans = event["spans"].as_array().expect("request association");
                let request = spans.iter().find(|span| span["route"] == match surface { "assignment" => "/api/v1/workforce/assignments", "termination" => "/api/v1/workforce/grants/{id}/terminations", "suspension" => "/api/v1/workforce/grants/{id}/suspensions", "resumption" => "/api/v1/workforce/suspensions/{id}/resumptions", _ => "/api/v1/workforce/grants" }).expect("original HTTP request");
                assert_eq!(request["operation_id"], OPERATION);
                let request_id = request["request_id"].as_str().expect("request ID");
                request_id.parse::<Uuid>().expect("server UUID");
                assert!(spans.iter().any(|span| span["family"] == match surface { "assignment" => "Kernel(WorkforceAssignment)", "termination" => "Kernel(GrantTermination)", "suspension" => "Kernel(GrantSuspension)", "resumption" => "Kernel(GrantResumption)", _ => "Kernel(Grant)" } && span["operation_id"] == OPERATION));
                let responses: Vec<_> = events.iter().filter(|event| event["fields"]["stage"] == "http_response" && event["spans"].as_array().is_some_and(|spans| spans.iter().any(|span| span["request_id"] == request_id))).collect();
                assert_eq!(responses.len(), 1);
                assert_eq!(responses[0]["fields"]["status"], if case == "unknown" { 503 } else { 201 });
                let cookies: Vec<String> = serde_json::from_slice(&std::fs::read(expected.path()).expect("actual generated cookies")).expect("cookie values");
                for secret in cookies.iter().map(String::as_str).chain(["13900000000", "13800138000", "00000020000229007X", "__Host-nucleus-session", "FictionalAccessKey", "ACS3-HMAC-SHA256", fixture::PASSWORD, "fictional-sensitive-termination-detail", "SELECT ", "INSERT "]) {
                    assert!(!events.iter().any(|event| contains_text(event, secret)), "private material in Workforce diagnostics");
                }
            }
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("production diagnostics filter");
    isolated_child(|| {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(40), async {
                let name = database(&server).await;
                let supplier = Supplier::echoing().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let application = Application::start(proxy_connections(&name, &proxy), keys(0), services(8).expect("settings"), process::Settings::new(Duration::from_secs(10)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
                let (base, stop, task) = running(application).await;
                let manager = issued_session_for(&base, &mut sms, "13900000000").await;
                let ordinary = issued_session_for(&base, &mut sms, "13800138000").await;
                let cookies = [&manager, &ordinary].map(|identity| identity.1.split_once('=').expect("cookie pair").1);
                std::fs::write(std::env::var_os("NUCLEUS_TEST_GRANT_DIAGNOSTICS").expect("private parent file"), serde_json::to_vec(&cookies).expect("cookie expectations")).expect("save private expectations");
                let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(15)).build().expect("client");
                let request = |path: &str, operation: &str, source: u8| client.post(format!("{base}{path}"))
                    .header("operation-id", operation).header("session-id", &manager.0).header("cookie", &manager.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.136.{source}"));
                let response = request("/api/v1/workforce/grants/candidates", &Uuid::new_v4().to_string(), 1).json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate");
                assert_eq!(response.status(), 200);
                let mut candidate = response.json::<serde_json::Value>().await.expect("confirmation");
                let surface = std::env::var("NUCLEUS_TEST_GRANT_SURFACE").expect("selected surface");
                let assignment = surface == "assignment";
                let suspension = surface == "suspension";
                let resumption = surface == "resumption";
                let termination = surface == "termination";
                let kind = if suspension || resumption { "full" } else { "default" };
                let role = candidate["roles"].as_array().expect("choices").iter().find(|role|role["kind"]==kind).expect("selected system role");
                let mut body = serde_json::json!({"entry":"institution","target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":null,"grant":null},"role":{"id":role["id"],"changeId":role["changeId"]}});
                let path = if suspension || resumption {
                    let response=request("/api/v1/workforce/grants",&Uuid::new_v4().to_string(),4).json(&body).send().await.expect("real domain administrator");assert_eq!(response.status(),201);
                    let domain=|path:&str,source:u8| client.post(format!("{base}{path}")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&ordinary.0).header("cookie",&ordinary.1).header("origin","https://example.com").header("x-real-ip",format!("203.0.148.{source}"));
                    let response=domain("/api/v1/workforce/grants/candidates",1).json(&serde_json::json!({"entry":"domain","mobile":"13900000000"})).send().await.expect("ordinary member candidate");assert_eq!(response.status(),200);
                    candidate=response.json::<serde_json::Value>().await.expect("domain confirmation");
                    let target=serde_json::json!({"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13900000000"});
                    let mut confirmation=target.clone();confirmation["assignmentId"]=candidate["assignmentId"].clone();confirmation["suspensionId"]=serde_json::Value::Null;confirmation["grant"]=serde_json::Value::Null;
                    let response=domain("/api/v1/workforce/grants",2).json(&serde_json::json!({"entry":"domain","target":confirmation,"role":{"id":candidate["roles"][0]["id"],"changeId":candidate["roles"][0]["changeId"]}})).send().await.expect("ordinary domain member");assert_eq!(response.status(),201);
                    let grant=response.json::<serde_json::Value>().await.expect("grant");body=serde_json::json!({"target":target});
                    let path=format!("/api/v1/workforce/grants/{}/suspensions",grant["grant"]["id"].as_str().expect("grant ID"));
                    if resumption {
                        let response=domain(&path,3).json(&body).send().await.expect("actual pause before resumption fault");assert_eq!(response.status(),201);
                        let pause=response.json::<serde_json::Value>().await.expect("pause result");
                        format!("/api/v1/workforce/suspensions/{}/resumptions",pause["suspension"]["id"].as_str().expect("suspension ID"))
                    } else { path }
                } else if assignment {
                    let response = request("/api/v1/workforce/grants", &Uuid::new_v4().to_string(), 4).json(&body).send().await.expect("real member before reassignment");
                    assert_eq!(response.status(), 201);
                    let response = request("/api/v1/workforce/assignments/candidates", &Uuid::new_v4().to_string(), 5).json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("current member confirmation");
                    assert_eq!(response.status(), 200);
                    candidate = response.json::<serde_json::Value>().await.expect("assignment candidate");
                    let role = candidate["roles"].as_array().expect("choices").iter().find(|role| role["kind"] == "full").expect("replacement system role");
                    body = serde_json::json!({"entry":"institution","target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13800138000","grantId":candidate["grantId"],"assignmentId":candidate["assignmentId"]},"role":{"id":role["id"],"changeId":role["changeId"]}});
                    "/api/v1/workforce/assignments".to_owned()
                } else if termination {
                    let response = request("/api/v1/workforce/grants", &Uuid::new_v4().to_string(), 4).json(&body).send().await.expect("admit before exit fault");
                    assert_eq!(response.status(),201);
                    let grant = response.json::<serde_json::Value>().await.expect("original grant");
                    format!("/api/v1/workforce/grants/{}/terminations", grant["grant"]["id"].as_str().expect("grant ID"))
                } else { "/api/v1/workforce/grants".to_owned() };
                let write = |source| if termination || suspension || resumption {
                    let request=client.post(format!("{base}{path}")).header("operation-id",OPERATION).header("session-id",&ordinary.0).header("cookie",&ordinary.1)
                        .header("origin","https://example.com").header("x-real-ip",format!("203.0.137.{source}"));
                    if suspension || resumption { request.json(&body) } else { request }
                } else { request(&path,OPERATION,source).json(&body) };
                let unknown = std::env::var("NUCLEUS_TEST_GRANT_CASE").expect("selected case") == "unknown";
                proxy.arm(if unknown { Fault::UnknownCommit } else { Fault::DelayedCommit });
                let pending = write(2);
                let waiter = tokio::spawn(async move { pending.send().await });
                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original business COMMIT intercepted");
                if !unknown { proxy.arm(Fault::ReadCommitTermination); proxy.release(); }
                let response = timeout(Duration::from_secs(15), waiter).await.expect("HTTP result deadline").expect("request task").expect("actual response");
                assert_eq!(response.status(), if unknown { 503 } else { 201 });
                assert_eq!(response.headers()["cache-control"], "no-store");
                let result = response.json::<serde_json::Value>().await.expect("safe response");
                if unknown { assert_eq!(result, serde_json::json!({"code":"outcome_unknown"})); proxy.release(); }
                else { timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("actual recovery termination fault"); assert_eq!(result["code"], "ok"); }
                let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
                let person: Uuid = candidate["identity"]["personId"].as_str().expect("person").parse().expect("UUID");
                observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                observer.client().query_one("SELECT id FROM identity.accounts WHERE person_id=$1 FOR UPDATE", &[&person]).await.expect("original transaction released its target Account lock");
                let ended = i64::from(termination); let paused = i64::from(suspension || resumption); let resumed = i64::from(resumption); let assigned = i64::from(assignment);
                assert!(observer.client().query_one("SELECT sequence=1 AND (SELECT count(*) FROM access.grants)=1+$3::bigint AND (SELECT count(*) FROM access.assignments)=1+$3::bigint+$5::bigint AND (SELECT count(*) FROM access.grants WHERE termination IS NULL)=1-$2::bigint+$3::bigint AND (SELECT count(*) FROM access.terminations)=$2 AND (SELECT count(*) FROM access.suspensions)=$3 AND (SELECT count(*) FROM access.resumptions)=$4 FROM identity.persons WHERE id=$1", &[&person,&ended,&paused,&resumed,&assigned]).await.expect("one committed graph before replay").get::<_,bool>(0));
                observer.client().batch_execute("COMMIT").await.expect("observation complete");
                let response = write(3).send().await.expect("explicit original replay");
                assert_eq!(response.status(), 201);
                let replay = response.json::<serde_json::Value>().await.expect("permanent result");
                if termination { assert_eq!(replay["termination"]["mode"], "self"); }
                else if suspension { assert_eq!(replay["suspension"]["sequence"],2); }
                else if resumption { assert_eq!(replay["resumption"]["sequence"],3); }
                else if assignment {
                    assert_eq!(replay["assignment"]["sequence"],2);
                    assert_eq!(replay["assignment"]["grantId"],candidate["grantId"]);
                    let operation: Uuid = OPERATION.parse().expect("operation UUID");
                    let row = observer.client().query_one("SELECT assignment.id, member.assignment_id, person.sequence AS person_sequence, (SELECT count(*) FROM access.assignments) AS assignments FROM access.assignments assignment JOIN access.grants member ON member.id=assignment.grant_id AND member.termination IS NULL JOIN identity.persons person ON person.id=assignment.person_id WHERE assignment.operation_id=$1 AND assignment.entry IS NOT NULL", &[&operation]).await.expect("same permanent assignment after replay");
                    assert_eq!(row.get::<_,i64>("assignments"),2);
                    assert_eq!(row.get::<_,i64>("person_sequence"),1);
                    assert_eq!(row.get::<_,Uuid>("id"),row.get::<_,Uuid>("assignment_id"));
                    assert_eq!(replay["assignment"]["id"],row.get::<_,Uuid>("id").to_string());
                }
                else { assert_eq!(replay["grant"]["status"], "active"); }
                if !unknown { assert_eq!(replay, result); }
                stop.send(()).expect("stop"); let (application, ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
                assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err(), "no supplier call through replay and drain");
                observer.terminate().await.expect("observer ended"); sms.stop().await; proxy.stop().await;
            }).await.expect("Workforce diagnostics deadline");
        });
    });
}
