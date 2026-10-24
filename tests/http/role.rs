use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*};

#[test]
fn institutional_role_operations_use_real_credentials_and_permanent_original_definitions() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(30), async {
            let name = empty_database(&server).await;
            let plan = initialize(&server, &name, "13900000000").await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(),sms.authority())).await.expect("verified startup");
            let (base, stop, task) = running(application).await;
            let administrator = issued_session_for(&base,&mut sms,"13900000000").await;
            let ordinary = issued_session_for(&base,&mut sms,"13800138000").await;
            let observer = connections(&server,&name,"clerk").recover().await.expect("independent runtime observer");
            let operation: Uuid = observer.client().query_one("SELECT operation_id FROM access.appointments WHERE source='initialization'",&[]).await.expect("initialization family").get(0);
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let request = |identity: &(String,String), operation: Uuid, source: u8| client.post(format!("{base}/api/v1/roles"))
                .header("session-id",&identity.0).header("cookie",&identity.1).header("operation-id",operation.to_string())
                .header("x-real-ip",format!("203.0.113.{source}"));
            let input = serde_json::json!({"name":"  Cafe\u{301}  ","level":300,"permissions":["institution.roles","institution.accounts"]});
            let response = request(&administrator,operation,1).header("origin","https://example.com").json(&input).send().await.expect("create");
            assert_eq!(response.status(),201);
            assert_eq!(response.headers()["cache-control"],"no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            let created = response.json::<serde_json::Value>().await.expect("original definition");
            assert_eq!(created["code"],"ok");
            let role = &created["role"];
            assert_eq!(role.as_object().expect("six fields").len(),6);
            assert_eq!(role["kind"],"custom"); assert_eq!(role["name"],"Café"); assert_eq!(role["level"],300);
            assert_eq!(role["permissions"],serde_json::json!(["institution.accounts","institution.roles"]));
            let id: Uuid = role["id"].as_str().expect("role UUID").parse().expect("UUID");
            const FACTS: &str = "SELECT jsonb_build_object('role',to_jsonb(role),'change',to_jsonb(change),'permissions',(SELECT jsonb_agg(permission ORDER BY capability_id) FROM access.permissions permission WHERE role_id=role.id),'entries',(SELECT jsonb_agg(entry ORDER BY capability_id) FROM access.entries entry WHERE change_id=change.id))::text FROM access.roles role JOIN access.changes change ON change.id=role.change_id WHERE role.id=$1";
            let original: String = observer.client().query_one(FACTS,&[&id]).await.expect("independent four facts before replay").get(0);
            let canonical = serde_json::json!({"name":"Café","level":300,"permissions":["institution.accounts","institution.roles"]});
            let response = request(&administrator,operation,2).header("origin","https://example.com").json(&canonical).send().await.expect("canonical replay");
            assert_eq!(response.status(),201); assert_eq!(response.json::<serde_json::Value>().await.expect("replay"),created);
            for (identity, body, status, code, source) in [
                (&ordinary,serde_json::json!({"name":"Denied","level":200,"permissions":[]}),403,"permission_denied",3),
                (&ordinary,serde_json::json!({"name":" ","level":200,"permissions":[]}),422,"invalid_input",4),
                (&administrator,serde_json::json!({"name":"Changed","level":300,"permissions":[]}),409,"request_mismatch",5),
                (&administrator,serde_json::json!({"name":" ","level":300,"permissions":[]}),422,"invalid_input",6),
            ] {
                let response = request(identity,operation,source).header("origin","https://example.com").json(&body).send().await.expect("typed input precedes business and original binding");
                assert_eq!(response.status(),status); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("failure"),serde_json::json!({"code":code}));
            }
            for (body,status,code,source) in [
                (canonical.clone(),409,"role_name_taken",7),
                (serde_json::json!({"name":"System","level":1,"permissions":[]}),403,"permission_denied",8),
                (serde_json::json!({"name":"System","level":1000,"permissions":[]}),403,"permission_denied",9),
                (serde_json::json!({"name":"Invalid","level":1001,"permissions":[]}),422,"invalid_input",10),
                (serde_json::json!({"name":"Invalid","level":200,"permissions":["workforce.roles"]}),422,"invalid_input",11),
                (serde_json::json!({"name":"Invalid","level":200,"permissions":["institution.roles","institution.roles"]}),422,"invalid_input",12),
            ] {
                let response = request(&administrator,Uuid::new_v4(),source).header("origin","https://example.com").json(&body).send().await.expect("new request rule");
                assert_eq!(response.status(),status); assert_eq!(response.json::<serde_json::Value>().await.expect("failure"),serde_json::json!({"code":code}));
            }
            for (builder,status) in [
                (request(&administrator,Uuid::new_v4(),13).header("sec-fetch-site","same-origin").json(&canonical),403),
                (request(&administrator,Uuid::new_v4(),14).header("origin","https://example.com").header("origin","https://example.com").json(&canonical),403),
                (request(&administrator,Uuid::new_v4(),15).header("origin","https://example.com").header("authorization","Bearer fictional").json(&canonical),400),
                (request(&administrator,Uuid::new_v4(),16).header("origin","https://example.com").body("{}"),415),
                (request(&administrator,Uuid::new_v4(),17).header("origin","https://example.com").header("content-type","application/json").body(" ".repeat(8193)),413),
            ] {
                let response = builder.send().await.expect("HTTP input boundary");
                assert_eq!(response.status(),status); assert!(!response.headers().contains_key("set-cookie"));
            }
            let after: String = observer.client().query_one(FACTS,&[&id]).await.expect("no denial changed original facts").get(0);
            assert_eq!(after,original);
            let response = client.get(format!("{base}/api/v1/roles/changes?operationId={operation}")).header("operation-id",OTHER).header("session-id",&administrator.0).header("cookie",&administrator.1).header("origin","https://example.com").header("x-real-ip","203.0.113.19").send().await.expect("history filters both operation families");
            assert_eq!(response.status(),200);
            let history = response.json::<serde_json::Value>().await.expect("permanent history");
            assert_eq!(history["count"],3);
            assert!(history["changes"].as_array().expect("changes").iter().any(|change|change["id"]==role["changeId"]&&change["name"]=="Café"&&change["operator"]["name"]=="F*"));
            let modification = Uuid::new_v4();
            let replacement = serde_json::json!({"changeId":role["changeId"],"name":"Revised café","level":301,"permissions":["institution.accounts"]});
            // This protocol matrix shares the real 10/s process write budget.
            let mut cadence=tokio::time::interval_at(tokio::time::Instant::now()+Duration::from_millis(120),Duration::from_millis(120));
            let update = |identity: Option<&(String,String)>, target: String, operation: Uuid, source: u8| {
                let mut request=client.put(format!("{base}/api/v1/roles/{target}")).header("operation-id",operation.to_string()).header("x-real-ip",format!("203.0.114.{source}"));
                if let Some(identity)=identity { request=request.header("session-id",&identity.0).header("cookie",&identity.1); }
                request
            };
            for (builder,status,code) in [
                (update(None,"not-a-uuid".into(),modification,1).body("{"),400,"invalid_input"),
                (update(None,id.to_string(),modification,2).json(&serde_json::json!({"changeId":"bad"})),401,"authentication_invalid"),
                (update(Some(&administrator),id.to_string(),modification,3).json(&serde_json::json!({"changeId":"bad"})),403,"permission_denied"),
                (update(Some(&ordinary),id.to_string(),modification,4).header("origin","https://example.com").json(&serde_json::json!({"changeId":"bad"})),422,"invalid_input"),
                (update(Some(&ordinary),id.to_string(),modification,5).header("origin","https://example.com").json(&replacement),403,"permission_denied"),
                (update(Some(&administrator),id.to_string(),modification,6).header("origin","https://example.com").header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
            ] {
                cadence.tick().await;
                let response=builder.send().await.expect("PUT input ordering");
                assert_eq!(response.status(),status); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("common JSON failure"),serde_json::json!({"code":code}));
            }
            replace_role(&base, &administrator, "13800138000", role, 1).await;
            let read_role=|path:&str|client.get(format!("{base}{path}")).header("operation-id",OTHER).header("session-id",&ordinary.0).header("cookie",&ordinary.1).header("origin","https://example.com").header("x-real-ip","203.0.114.23");
            assert_eq!(read_role("/api/v1/accounts/current/role").send().await.expect("real holder reads original").json::<serde_json::Value>().await.expect("original role")["role"],*role);
            assert_eq!(read_role("/api/v1/roles").send().await.expect("holder originally has roles permission").status(),200);
            cadence.tick().await;
            let response=update(Some(&administrator),id.to_string(),modification,7).header("origin","https://example.com").json(&replacement).send().await.expect("replace role");
            assert_eq!(response.status(),200);
            let revised=response.json::<serde_json::Value>().await.expect("updated definition");
            assert_eq!(revised["role"].as_object().expect("six fields").len(),6);
            assert_eq!(revised["role"]["id"],role["id"]); assert_ne!(revised["role"]["changeId"],role["changeId"]);
            assert_eq!(revised["role"]["name"],"Revised café"); assert_eq!(revised["role"]["permissions"],serde_json::json!(["institution.accounts"]));
            assert_eq!(read_role("/api/v1/accounts/current/role").send().await.expect("same holder immediately reads new definition").json::<serde_json::Value>().await.expect("current role")["role"],revised["role"]);
            assert_eq!(read_role("/api/v1/roles").send().await.expect("same holder immediately loses removed permission").status(),403);
            let saved:String=observer.client().query_one(FACTS,&[&id]).await.expect("independent updated group before replay").get(0);
            let rows=observer.client().query("SELECT action, name, level, (SELECT count(*) FROM access.entries WHERE change_id=change.id) FROM access.changes change WHERE role_id=$1 ORDER BY changed_at",&[&id]).await.expect("permanent predecessor and successor");
            assert_eq!(rows.len(),2); assert_eq!(rows[0].get::<_,String>(0),"create"); assert_eq!(rows[1].get::<_,String>(0),"update"); assert_eq!(rows[1].get::<_,i64>(3),1);
            let mut equivalent=replacement.clone();
            equivalent["changeId"]=serde_json::json!(Uuid::parse_str(role["changeId"].as_str().expect("change ID")).expect("UUID").simple().to_string().to_uppercase());
            cadence.tick().await;
            let response=update(Some(&administrator),id.simple().to_string().to_uppercase(),modification,8).header("origin","https://example.com").json(&equivalent).send().await.expect("UUID values bind original input");
            assert_eq!(response.status(),200); assert_eq!(response.json::<serde_json::Value>().await.expect("original update"),revised);
            for (target,operation,body,status,code,source) in [
                (id,Uuid::new_v4(),replacement.clone(),409,"role_changed",9),
                (Uuid::new_v4(),modification,replacement.clone(),409,"request_mismatch",10),
                (Uuid::new_v4(),Uuid::new_v4(),replacement.clone(),404,"target_unavailable",11),
                (id,Uuid::new_v4(),serde_json::json!({"changeId":revised["role"]["changeId"],"name":"Revised café","level":301,"permissions":["institution.accounts"]}),409,"state_conflict",12),
                (id,Uuid::new_v4(),serde_json::json!({"change_id":revised["role"]["changeId"],"name":"Invalid","level":301,"permissions":[]}),422,"invalid_input",13),
            ] {
                cadence.tick().await;
                let response=update(Some(&administrator),target.to_string(),operation,source).header("origin","https://example.com").json(&body).send().await.expect("confirmed role operation rules");
                assert_eq!(response.status(),status); assert_eq!(response.json::<serde_json::Value>().await.expect("rule failure"),serde_json::json!({"code":code}));
            }
            assert_eq!(observer.client().query_one(FACTS,&[&id]).await.expect("replays and failures preserve group").get::<_,String>(0),saved);
            let response=request(&administrator,operation,21).header("origin","https://example.com").json(&canonical).send().await.expect("creation replay after modification");
            assert_eq!(response.status(),201); assert_eq!(response.json::<serde_json::Value>().await.expect("original creation definition"),created);
            let response=client.get(format!("{base}/api/v1/roles")).header("operation-id",OTHER).header("session-id",&administrator.0).header("cookie",&administrator.1).header("origin","https://example.com").header("x-real-ip","203.0.114.22").send().await.expect("current list after modification");
            assert_eq!(response.status(),200); let current=response.json::<serde_json::Value>().await.expect("current roles");
            assert!(current["roles"].as_array().expect("roles").iter().any(|current|current==&revised["role"]));
            let deletion=Uuid::new_v4();
            let confirmation=serde_json::json!({"changeId":revised["role"]["changeId"]});
            let delete=|identity:Option<&(String,String)>,target:String,operation:Uuid,source:u8| {
                let mut request=client.delete(format!("{base}/api/v1/roles/{target}")).header("operation-id",operation.to_string()).header("x-real-ip",format!("203.0.115.{source}"));
                if let Some(identity)=identity {request=request.header("session-id",&identity.0).header("cookie",&identity.1);}
                request
            };
            for (builder,status,code) in [
                (delete(None,"invalid".into(),deletion,1).body("not JSON"),400,"invalid_input"),
                (delete(None,id.to_string(),deletion,2).json(&serde_json::json!({})),401,"authentication_invalid"),
                (delete(Some(&administrator),id.to_string(),deletion,3).header("sec-fetch-site","same-origin").json(&confirmation),403,"permission_denied"),
                (delete(Some(&ordinary),id.to_string(),deletion,4).header("origin","https://example.com").json(&serde_json::json!({})),422,"invalid_input"),
                (delete(Some(&ordinary),id.to_string(),deletion,5).header("origin","https://example.com").json(&confirmation),403,"permission_denied"),
                (delete(Some(&administrator),id.to_string(),deletion,6).header("origin","https://example.com").json(&serde_json::json!({"changeId":null})),422,"invalid_input"),
                (delete(Some(&administrator),id.to_string(),deletion,7).header("origin","https://example.com").json(&serde_json::json!({"changeId":revised["role"]["changeId"],"permissions":[]})),422,"invalid_input"),
                (delete(Some(&administrator),id.to_string(),deletion,8).header("origin","https://example.com").json(&serde_json::json!({"changeId":role["changeId"]})),409,"role_changed"),
                (delete(Some(&administrator),id.to_string(),deletion,9).header("origin","https://example.com").json(&confirmation),409,"role_in_use"),
                (delete(Some(&administrator),id.to_string(),deletion,10).header("origin","https://example.com").header("content-type","application/json").body(" ".repeat(8193)),413,"invalid_input"),
            ] {
                cadence.tick().await;let response=builder.send().await.expect("DELETE input and authorization order");
                assert_eq!(response.status(),status);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));assert_eq!(response.json::<serde_json::Value>().await.expect("common JSON failure"),serde_json::json!({"code":code}));
            }
            assert_eq!(observer.client().query_one(FACTS,&[&id]).await.expect("rejected deletion made no change").get::<_,String>(0),saved);
            let default = current["roles"].as_array().expect("current roles").iter().find(|role|role["kind"]=="default").expect("default role");
            replace_role(&base, &administrator, "13800138000", default, 2).await;
            cadence.tick().await;
            let response=delete(Some(&administrator),id.to_string(),deletion,11).header("origin","https://example.com").json(&confirmation).send().await.expect("delete unused custom role");
            assert_eq!(response.status(),200);assert!(!response.headers().contains_key("set-cookie"));
            let deleted=response.json::<serde_json::Value>().await.expect("permanent deletion");
            assert_eq!(deleted["code"],"ok");assert_eq!(deleted["change"]["roleId"],role["id"]);assert_eq!(deleted["change"]["action"],"delete");assert!(deleted["change"].get("permissions").is_none());
            assert_eq!(deleted["change"]["name"],revised["role"]["name"]);assert_eq!(deleted["change"]["operator"]["name"],"F*");assert_eq!(deleted["change"]["operator"].as_object().expect("only historical masked identity").len(),3);
            let deleted_id:Uuid=serde_json::from_value(deleted["change"]["id"].clone()).expect("terminal change ID");
            let row=observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.roles role JOIN access.changes change ON change.id=role.change_id WHERE role.id=$1 AND NOT role.active AND role.permissions='{}'::text[] AND role.scope='institution' AND role.change_id=$2 AND role.name=change.name AND role.level=change.level AND role.changed_at=change.changed_at AND role.sequence=3 AND role.version=3 AND change.sequence=role.sequence AND change.action='delete'),NOT EXISTS(SELECT 1 FROM access.permissions WHERE role_id=$1),(SELECT count(*) FROM access.changes WHERE role_id=$1),(SELECT count(*) FROM access.entries entry JOIN access.changes change ON change.id=entry.change_id WHERE role_id=$1 AND action='delete')",&[&id,&deleted_id]).await.expect("independent terminal facts before replay");
            assert!(row.get::<_,bool>(0));assert!(row.get::<_,bool>(1));assert_eq!(row.get::<_,i64>(2),3);assert_eq!(row.get::<_,i64>(3),0);
            cadence.tick().await;
            let current=client.get(format!("{base}/api/v1/roles")).header("operation-id",OTHER).header("session-id",&administrator.0).header("cookie",&administrator.1).header("origin","https://example.com").header("x-real-ip","203.0.115.32").send().await.expect("live definitions after deletion");
            assert_eq!(current.status(),200);
            assert!(!current.json::<serde_json::Value>().await.expect("live role page")["roles"].as_array().expect("definitions").iter().any(|value|value["id"]==role["id"]));
            cadence.tick().await;
            let response=delete(Some(&administrator),id.simple().to_string(),deletion,12).header("origin","https://example.com").json(&confirmation).send().await.expect("original delete after target absence");assert_eq!(response.status(),200);assert_eq!(response.json::<serde_json::Value>().await.expect("same deletion"),deleted);
            cadence.tick().await;
            let response=update(Some(&administrator),id.to_string(),modification,30).header("origin","https://example.com").json(&replacement).send().await.expect("old PUT after deletion");assert_eq!(response.status(),200);assert_eq!(response.json::<serde_json::Value>().await.expect("same old definition"),revised);
            cadence.tick().await;
            let response=request(&administrator,operation,31).header("origin","https://example.com").json(&canonical).send().await.expect("old POST after deletion");assert_eq!(response.status(),201);assert_eq!(response.json::<serde_json::Value>().await.expect("same creation"),created);
            let history=client.get(format!("{base}/api/v1/roles/changes?roleId={id}")).header("operation-id",OTHER).header("session-id",&administrator.0).header("cookie",&administrator.1).header("origin","https://example.com").header("x-real-ip","203.0.115.13").send().await.expect("terminal history").json::<serde_json::Value>().await.expect("history page");
            assert_eq!(history["count"],3);assert_eq!(history["changes"][0],deleted["change"]);
            for method in [reqwest::Method::GET,reqwest::Method::HEAD] {
                let response=client.request(method,format!("{base}/api/v1/roles/{id}")).header("operation-id",OTHER).send().await.expect("unsupported target method");
                assert_eq!(response.status(),405); let allow=response.headers()["allow"].to_str().expect("methods");assert!(allow.contains("PUT")&&allow.contains("DELETE"));
            }
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let removal=&document["paths"]["/api/v1/roles/{id}"]["delete"];
            assert_eq!(removal["operationId"],"deleteRole");assert_eq!(removal["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/role.Delete");
            assert_eq!(document["components"]["schemas"]["role.Delete"]["required"],serde_json::json!(["changeId"]));assert_eq!(document["components"]["schemas"]["role.Delete"]["properties"].as_object().expect("only confirmation input").len(),1);
            for status in ["200","400","401","403","404","409","413","415","422","429","500","503"] {assert!(removal["responses"][status]["headers"].get("Cache-Control").is_some(),"DELETE {status}");}
            let modification=&document["paths"]["/api/v1/roles/{id}"]["put"];
            assert_eq!(modification["operationId"],"updateRole");
            assert!(modification["parameters"].as_array().expect("parameters").iter().any(|parameter|parameter["name"]=="id"&&parameter["in"]=="path"&&parameter["required"]==true&&parameter["schema"]["format"]=="uuid"));
            assert_eq!(modification["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/role.Update");
            assert!(document["components"]["schemas"]["role.Update"]["required"].as_array().expect("required input").contains(&serde_json::json!("changeId")));
            for status in ["200","400","401","403","404","409","413","415","422","429","500","503"] { assert!(modification["responses"].get(status).is_some(),"{status}"); assert!(modification["responses"][status]["headers"].get("Cache-Control").is_some(),"PUT {status} cache policy"); }
            let definition = &document["paths"]["/api/v1/roles"]["post"];
            assert_eq!(definition["operationId"],"createRole");
            for header in ["Origin","Session-Id","Operation-Id"] { assert!(definition["parameters"].as_array().expect("headers").iter().any(|parameter|parameter["name"]==header&&parameter["required"]==true)); }
            for status in ["201","400","401","403","409","413","415","422","429","500","503"] { assert!(definition["responses"].get(status).is_some(),"{status}"); }
            assert_eq!(definition["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/role.Input");
            assert_eq!(document["components"]["schemas"]["role.Input"]["properties"]["permissions"]["$ref"],"#/components/schemas/role.Permissions");
            let permissions=&document["components"]["schemas"]["role.Permissions"];
            assert_eq!(permissions["uniqueItems"],true);
            let mut codes:Vec<_>=permissions["items"]["enum"].as_array().expect("institutional input codes").iter().map(|code|code.as_str().expect("code")).collect();
            codes.sort_unstable();
            assert_eq!(codes,["institution.accounts","institution.affiliations.correct","institution.affiliations.read","institution.assignments","institution.corrections","institution.roles","institution.workforce"]);
            stop.send(()).expect("stop"); let (mut application,result)=task.await.expect("listener ended"); result.expect("normal serve"); application.stop().await.expect("accepted work drains"); drop(application);
            assert!(!replay_initialization(&server,&name,"13900000000",&plan).await,"online UUID does not disturb initialization replay");
            let owner=connections(&server,&name,"postgres").recover().await.expect("isolated damaged history owner");
            owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated missing-state fixture");
            assert_eq!(owner.client().execute("DELETE FROM access.roles WHERE id=$1 AND NOT active",&[&id]).await.expect("remove only the sealed tombstone and retain its authentic records"),1);
            owner.client().batch_execute("COMMIT").await.expect("orphan sealed history visible");
            owner.terminate().await.expect("damage transaction ended");
            let startup=Application::start(connections(&server,&name,"clerk"),keys(0),services(4).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await;
            assert!(role_facts_rejected(&startup));
            assert!(instance_released(&server, &name).await,"failed startup releases resources without opening service");
            observer.terminate().await.expect("observer ended");
            assert!(timeout(Duration::from_millis(50),sms.request()).await.is_err(),"no extra PNVS through creation, replay and drain");
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2,"no extra Siteverify through creation, replay and drain");
            sms.stop().await; drop(supplier);
        }).await.expect("bounded role HTTP flow");
    });
}

#[test]
fn institutional_role_operations_drain_cancelled_http_waiters_and_log_once() {
    use fixture::database_proxy::{Fault, Proxy};
    if std::env::var_os("NUCLEUS_TEST_ROLE_CREATE_DIAGNOSTICS").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            for action in ["create", "update", "delete"] {
                let expected =
                    tempfile::NamedTempFile::new().expect("private actual token expectation");
                let child = tokio::process::Command::new(
                    std::env::current_exe().expect("test executable"),
                )
                .args([
                    "--exact",
                    "role::institutional_role_operations_drain_cancelled_http_waiters_and_log_once",
                    "--nocapture",
                ])
                .env("NUCLEUS_TEST_ROLE_ACTION", action)
                .env("NUCLEUS_TEST_ROLE_CREATE_DIAGNOSTICS", expected.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .expect("diagnostic child");
                let output = timeout(Duration::from_secs(35), child.wait_with_output())
                    .await
                    .expect("bounded child")
                    .expect("child output");
                let text = String::from_utf8(output.stderr).expect("JSON events");
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
                    "one actual accepted work, including cancelled caller: {text}"
                );
                let event = failures[0];
                assert_eq!(event["fields"]["operation_id"], OPERATION);
                assert_eq!(event["fields"]["stage"], "completion_recovery");
                assert_eq!(event["fields"]["outcome"], "committed");
                assert_eq!(event["target"], "domain::kernel::executor");
                assert_eq!(
                    event["spans"][0]["route"],
                    if action == "create" {
                        "/api/v1/roles"
                    } else {
                        "/api/v1/roles/{id}"
                    }
                );
                assert_eq!(event["spans"][0]["operation_id"], OPERATION);
                assert!(
                    event["spans"][0]["request_id"]
                        .as_str()
                        .expect("request association")
                        .parse::<Uuid>()
                        .is_ok()
                );
                assert!(
                    event["fields"]["error"]
                        .as_str()
                        .expect("original error")
                        .contains("stage: Commit, kind: Query(connection closed)")
                );
                assert!(
                    event["fields"]["error"]
                        .as_str()
                        .expect("actual recovery commit timeout")
                        .contains("(Commit, Timeout(Elapsed")
                );
                let token =
                    std::fs::read_to_string(expected.path()).expect("actual generated token");
                for secret in [
                    token.as_str(),
                    "private-created-role",
                    "13900000000",
                    "__Host-nucleus-session",
                    "FictionalAccessKey",
                    "ACS3-HMAC-SHA256",
                    fixture::PASSWORD,
                    "SELECT ",
                    "INSERT ",
                ] {
                    assert!(
                        !events.iter().any(|event| contains_text(event, secret)),
                        "private material in final diagnostics"
                    );
                }
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("real diagnostic filters");
    isolated_child(|| {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30),async {
                let name=database(&server).await;
                let supplier=Supplier::echoing().await;let mut sms=fixture::supplier::Supplier::start().await;
                let proxy=Proxy::start(&server).await;
                let application=Application::start(proxy_connections(&name,&proxy),keys(0),services(4).expect("settings"),process::Settings::new(Duration::from_secs(20)).expect("stop budget"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("application");
                let (base,stop,mut task)=running(application).await;
                let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
                timeout(Duration::from_secs(3),async {loop {
                    if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND usename='clerk' AND state='idle' AND query LIKE 'SELECT family, operation_id, action, target, input, subject, person_id, revision_id, created_at, expires_at, seal, seal_key_id, seal_version FROM workforce.completions WHERE expires_at <= %')",&[]).await.expect("empty first background round").get::<_,bool>(0) {break;} tokio::task::yield_now().await;
                }}).await.expect("fault cannot target initial background work");
                let administrator=issued_session_for(&base,&mut sms,"13900000000").await;
                std::fs::write(std::env::var_os("NUCLEUS_TEST_ROLE_CREATE_DIAGNOSTICS").expect("private parent-owned file"),administrator.1.split_once('=').expect("cookie pair").1).expect("record token only in parent private fixture");
                let mut input=serde_json::json!({"name":"private-created-role","level":200,"permissions":["institution.accounts"]});
                let action=std::env::var("NUCLEUS_TEST_ROLE_ACTION").expect("scenario");
                let (method,path,status)=if action!="create" {
                    let response=http_client().post(format!("{base}/api/v1/roles")).header("operation-id",OTHER).header("session-id",&administrator.0).header("cookie",&administrator.1).json(&serde_json::json!({"name":"Before change","level":200,"permissions":[]})).send().await.expect("create target before fault");
                    assert_eq!(response.status(),201);let original=response.json::<serde_json::Value>().await.expect("target");input["changeId"]=original["role"]["changeId"].clone();
                    let method=if action=="delete" {input=serde_json::json!({"changeId":original["role"]["changeId"]});reqwest::Method::DELETE}else{reqwest::Method::PUT};
                    (method,format!("/api/v1/roles/{}",original["role"]["id"].as_str().expect("target ID")),200)
                } else {(reqwest::Method::POST,"/api/v1/roles".into(),201)};
                proxy.arm(Fault::DelayedCommit);
                let caller=tokio::spawn(http_client().request(method.clone(),format!("{base}{path}")).header("operation-id",OPERATION).header("session-id",&administrator.0).header("cookie",&administrator.1).timeout(Duration::from_secs(25)).json(&input).send());
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("role write COMMIT held");
                let (original,recovery)=timeout(Duration::from_secs(3),async {loop {
                    if let Some(row)=observer.client().query_opt("SELECT holder.pid,waiter.pid FROM pg_locks waiter JOIN pg_locks holder USING(locktype,database,classid,objid,objsubid) WHERE waiter.locktype='advisory' AND NOT waiter.granted AND holder.granted AND waiter.mode='ExclusiveLock' AND holder.mode='ExclusiveLock' AND waiter.classid=602503669::oid AND waiter.objid=1284585832::oid AND waiter.objsubid=1 AND waiter.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND holder.pid=ANY(pg_blocking_pids(waiter.pid))",&[]).await.expect("independently derived nil Role operation lock") {break(row.get::<_,i32>(0),row.get::<_,i32>(1));} tokio::task::yield_now().await;
                }}).await.expect("readback waits for actual original backend");
                proxy.arm(Fault::StalledReadCommit);proxy.release();
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("complete original read followed by withheld COMMIT reply");
                const FACTS:&str="SELECT jsonb_build_object('role',to_jsonb(role),'change',to_jsonb(change),'permissions',(SELECT jsonb_agg(permission ORDER BY capability_id) FROM access.permissions permission WHERE role_id=change.role_id),'entries',(SELECT jsonb_agg(entry ORDER BY capability_id) FROM access.entries entry WHERE change_id=change.id))::text FROM access.changes change LEFT JOIN access.roles role ON role.id=change.role_id WHERE change.operation_id='00000000-0000-0000-0000-000000000000'";
                let before:String=observer.client().query_one(FACTS,&[]).await.expect("independent four facts before cancellation or replay").get(0);
                caller.abort();assert!(caller.await.expect_err("cancel HTTP waiter").is_cancelled());
                stop.send(()).expect("stop while accepted work remains");
                assert!(timeout(Duration::from_millis(30),&mut task).await.is_err(),"stop must await actual accepted readback cleanup");
                assert!(instance_held(&server, &name).await,"ownership survives waiter cancellation");
                let (application,result)=loop {tokio::select! {
                    result=&mut task=>break result.expect("process task"),
                    request=timeout(Duration::from_secs(4),sms.request())=>assert!(request.is_err(),"no supplier action during readback and drain"),
                }};
                result.expect("accepted work drains within stop budget");drop(application);proxy.release();
                timeout(Duration::from_secs(3),async {loop {
                    if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=ANY($1::int[]))",&[&vec![original,recovery]]).await.expect("remote backend end").get::<_,bool>(0) {break;} tokio::task::yield_now().await;
                }}).await.expect("actual original and recovery backends end");
                let after:String=observer.client().query_one(FACTS,&[]).await.expect("independent original before restart").get(0);assert_eq!(before,after);
                let application=Application::start(connections(&server,&name,"clerk"),keys(0),services(4).expect("settings"),process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("complete role passes startup verification");
                let (base,stop,task)=running(application).await;
                let response=http_client().request(method,format!("{base}{path}")).header("operation-id",OPERATION).header("session-id",&administrator.0).header("cookie",&administrator.1).json(&input).send().await.expect("original request after process lifecycle restart");
                assert_eq!(response.status(),status);assert!(!response.headers().contains_key("set-cookie"));
                let delivered=response.json::<serde_json::Value>().await.expect("original definition");let facts:serde_json::Value=serde_json::from_str(&before).expect("observed graph");
                if action=="delete" {
                    assert_eq!(facts["role"]["id"],facts["change"]["role_id"]);assert_eq!(facts["role"]["active"],false);
                    assert_eq!(facts["role"]["permissions"],serde_json::json!([]));assert_eq!(facts["role"]["change_id"],facts["change"]["id"]);
                    assert_eq!(facts["role"]["sequence"],2);assert_eq!(facts["role"]["version"],2);
                    assert!(facts["permissions"].is_null());assert!(facts["entries"].is_null());
                    assert_eq!(delivered["change"]["id"],facts["change"]["id"]);assert_eq!(delivered["change"]["action"],"delete");assert!(delivered["change"].get("permissions").is_none());
                } else {assert_eq!(delivered["role"]["id"],facts["role"]["id"]);assert_eq!(delivered["role"]["changeId"],facts["change"]["id"]);}
                stop.send(()).expect("stop replay");let (application,result)=task.await.expect("process");result.expect("normal replay drain");drop(application);
                assert_eq!(supplier.calls.load(Ordering::SeqCst),1);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err());
                observer.terminate().await.expect("observer ended");sms.stop().await;proxy.stop().await;
            }).await.expect("bounded actual HTTP cancellation");
        });
    });
}

#[test]
fn institutional_roles_read_real_accounts_and_report_safe_query_failures() {
    if std::env::var_os("NUCLEUS_TEST_ROLE_DIAGNOSTICS").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let child = tokio::process::Command::new(
                std::env::current_exe().expect("test executable"),
            )
            .args([
                "--exact",
                "role::institutional_roles_read_real_accounts_and_report_safe_query_failures",
                "--nocapture",
            ])
            .env("NUCLEUS_TEST_ROLE_DIAGNOSTICS", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("isolated logging process");
            let output = timeout(Duration::from_secs(40), child.wait_with_output())
                .await
                .expect("child budget")
                .expect("child result");
            let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
            assert!(output.status.success(), "{text}");
            let events: Vec<serde_json::Value> = text
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON event"))
                .collect();
            let failures: Vec<_> = events
                .iter()
                .filter(|event| event["level"] == "ERROR")
                .collect();
            assert_eq!(
                failures.len(),
                6,
                "one safe event per failed request: {text}"
            );
            for (failure, (stage, route, operation)) in failures.iter().zip([
                ("access_read", "/api/v1/accounts/current/role", OTHER),
                ("access_read", "/api/v1/roles", OTHER),
                ("access_read", "/api/v1/roles/changes", OTHER),
                (
                    "access_read",
                    "/api/v1/accounts/current/role",
                    "00000000-0000-4000-a000-000000000001",
                ),
                (
                    "readiness",
                    "/health/ready",
                    "00000000-0000-4000-a000-000000000002",
                ),
                (
                    "current_account",
                    "/api/v1/roles",
                    "00000000-0000-4000-a000-000000000003",
                ),
            ]) {
                assert_eq!(failure["fields"]["stage"], stage);
                assert_eq!(failure["spans"][0]["route"], route);
                assert_eq!(failure["spans"][0]["operation_id"], operation);
                let request = &failure["spans"][0]["request_id"];
                assert!(
                    events
                        .iter()
                        .any(|event| event["fields"]["stage"] == "http_response"
                            && event["fields"]["status"] == 503
                            && event["spans"].as_array().is_some_and(|spans| spans
                                .iter()
                                .any(|span| &span["request_id"] == request))),
                    "failure has one correlated unavailable response"
                );
            }
            assert!(
                text.contains("42501"),
                "the actual PostgreSQL query cause remains available"
            );
            for secret in [
                "13900000000",
                "13800138000",
                "Fixture Registered",
                "__Host-nucleus-session",
                "SELECT ",
                "private-role-query",
            ] {
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
            timeout(Duration::from_secs(30), async {
                let name = empty_database(&server).await;
                initialize(&server, &name, "13900000000").await;
                let supplier = Supplier::echoing().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                    services(8).expect("settings"),
                    process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("start");
                let (base, stop, task) = running(application).await;
                let administrator = issued_session_for(&base, &mut sms, "13900000000").await;
                let ordinary = issued_session_for(&base, &mut sms, "13800138000").await;
                let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("HTTP client");
                let get = |id: &str, cookie: &str| client.get(format!("{base}/api/v1/accounts/current/role"))
                    .header("operation-id", OTHER).header("session-id", id).header("cookie", cookie)
                    .header("sec-fetch-site", "same-origin").header("x-real-ip", "203.0.113.85");
                let list = |id: &str, cookie: &str, query: &str| client.get(format!("{base}/api/v1/roles{query}"))
                    .header("operation-id", OTHER).header("session-id", id).header("cookie", cookie)
                    .header("sec-fetch-site", "same-origin").header("x-real-ip", "203.0.113.86");
                let history = |id: &str, cookie: &str, query: &str| client.get(format!("{base}/api/v1/roles/changes{query}"))
                    .header("operation-id", OTHER).header("session-id", id).header("cookie", cookie)
                    .header("sec-fetch-site", "same-origin").header("x-real-ip", "203.0.113.87");
                let observer = connections(&server, &name, "clerk").recover().await.expect("independent runtime observer");
                let observed_at: chrono::DateTime<chrono::Utc> = observer.client().query_one("SELECT observed_at FROM security.clocks WHERE id", &[]).await.expect("clock before unsupported methods").get(0);
                for method in [reqwest::Method::POST,reqwest::Method::PUT,reqwest::Method::PATCH,reqwest::Method::DELETE,reqwest::Method::OPTIONS] {
                    for headers in [vec![],vec![("x-real-ip","invalid"),("session-id","invalid"),("authorization","private-role-query")],vec![("x-real-ip","203.0.113.85"),("session-id",ordinary.0.as_str()),("cookie",ordinary.1.as_str()),("origin","https://example.com")]] {
                        let mut request = client.request(method.clone(),format!("{base}/api/v1/accounts/current/role")).header("operation-id",OTHER);
                        for (name,value) in headers { request=request.header(name,value); }
                        let response=request.send().await.expect("unsupported method through production assembly");
                        assert_eq!(response.status(),405);
                        let mut allowed: Vec<_> = response.headers()["allow"].to_str().expect("Allow").split(',').map(str::trim).collect(); allowed.sort_unstable();
                        assert_eq!(allowed,["GET","HEAD"]);
                        assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
                        assert_eq!(response.json::<serde_json::Value>().await.expect("method failure"),serde_json::json!({"code":"method_not_allowed"}));
                    }
                }
                assert_eq!(observer.client().query_one("SELECT observed_at FROM security.clocks WHERE id", &[]).await.expect("unsupported methods do not authenticate").get::<_,chrono::DateTime<chrono::Utc>>(0),observed_at);
                assert_eq!(client.post(format!("{base}/api/v1/accounts/current/role")).send().await.expect("global request identity still applies").status(),400);
                for ((id, cookie), kind, level, expected_name, permissions) in [
                    (&administrator, "full", 1000, "系统管理员", vec!["institution.accounts", "institution.affiliations.correct", "institution.affiliations.read", "institution.assignments", "institution.corrections", "institution.roles", "institution.workforce"]),
                    (&ordinary, "default", 1, "普通账户", vec![]),
                ] {
                    let facts = observer.client().query_one("SELECT holder.role_id::text,role.change_id::text,ARRAY(SELECT capability_id FROM access.permissions WHERE role_id=role.id ORDER BY capability_id COLLATE \"C\") AS permissions FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN access.holders holder ON holder.id=account.id JOIN access.appointments appointment ON appointment.id=holder.assignment_id AND appointment.person_id=holder.person_id AND appointment.role_id=holder.role_id JOIN access.roles role ON role.id=holder.role_id WHERE session.id=$1::text::uuid", &[id]).await.expect("independent role facts before HTTP read");
                    let response = get(id, cookie).send().await.expect("current role GET");
                    assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("complete role"), serde_json::json!({"code":"ok","role":{"id":facts.get::<_,String>("role_id"),"kind":kind,"name":expected_name,"level":level,"permissions":permissions,"changeId":facts.get::<_,String>("change_id")}}));
                    assert_eq!(facts.get::<_,Vec<String>>("permissions"), permissions);
                    let response = client.head(format!("{base}/api/v1/accounts/current/role")).header("operation-id", OTHER).header("session-id", id).header("cookie", cookie).header("origin", "https://example.com").header("x-real-ip", "203.0.113.85").send().await.expect("current role HEAD");
                    assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie")); assert!(response.bytes().await.expect("HEAD bytes").is_empty());
                }
                for (headers, status) in [
                    (vec![("authorization", "private-role-query")], 400),
                    (vec![("origin", "https://wrong.invalid")], 403),
                    (vec![("session-id", OTHER)], 400),
                ] {
                    let mut request = get(&ordinary.0, &ordinary.1);
                    for (name, value) in headers { request = request.header(name, value); }
                    let response = request.send().await.expect("shared credential boundary");
                    assert_eq!(response.status(), status); assert!(!response.headers().contains_key("set-cookie"));
                }
                let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
                let resolve = |schema: &serde_json::Value| document.pointer(schema["$ref"].as_str().expect("reference").strip_prefix('#').expect("local reference")).expect("schema");
                let operation = &document["paths"]["/api/v1/accounts/current/role"]["get"];
                assert!(operation["parameters"].as_array().expect("parameters").iter().all(|parameter| parameter["name"] != "Origin"));
                assert!(operation["responses"]["429"]["headers"].get("Retry-After").is_some());
                let receipt = resolve(&operation["responses"]["200"]["content"]["application/json"]["schema"]);
                let role = resolve(&receipt["properties"]["role"]);
                assert_eq!(role["properties"].as_object().expect("role fields").len(), 6);
                assert_eq!(role["required"], serde_json::json!(["id","kind","name","level","permissions","changeId"]));
                for field in ["id", "changeId"] { assert_eq!(resolve(&role["properties"][field])["format"], "uuid"); }
                assert_eq!(resolve(&role["properties"]["kind"])["enum"], serde_json::json!(["default","full","custom"]));
                assert_eq!(role["properties"]["permissions"]["items"]["$ref"], "#/components/schemas/capability.Code");
                assert!(document["paths"]["/api/v1/accounts/current/role"]["head"]["responses"]["200"].get("content").is_none());
                let response = list(&ordinary.0, &ordinary.1, "").send().await.expect("ordinary account has no management permission");
                assert_eq!(response.status(), 403);
                assert_eq!(response.json::<serde_json::Value>().await.expect("denial"), serde_json::json!({"code":"permission_denied"}));
                let response = list(&administrator.0, &administrator.1, "").send().await.expect("default page");
                assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                let all = response.json::<serde_json::Value>().await.expect("role list");
                assert_eq!(all["code"], "ok"); assert_eq!(all["count"], 2);
                assert_eq!(all["roles"].as_array().expect("definitions").len(), 2);
                assert_eq!(all["roles"][0]["kind"], "full"); assert_eq!(all["roles"][1]["kind"], "default");
                assert_eq!(all["roles"][0].as_object().expect("six fields").len(), 6);
                for (query, expected) in [("?size=1&page=2", serde_json::json!([all["roles"][1]])), ("?page=9223372036854775807&size=1", serde_json::json!([]))] {
                    let response = list(&administrator.0, &administrator.1, query).send().await.expect("selected page");
                    assert_eq!(response.status(), 200);
                    assert_eq!(response.json::<serde_json::Value>().await.expect("page"), serde_json::json!({"code":"ok","count":2,"roles":expected}));
                }
                for query in ["?page=0","?page=-1","?size=0","?size=101","?page=1.5","?page=","?size=", "?page=1&page=2","?entry=institution","?page=9223372036854775807&size=100","?page=9223372036854775808"] {
                    let response = list(&administrator.0, &administrator.1, query).send().await.expect("invalid pagination");
                    assert_eq!(response.status(), 400, "{query}");
                    assert_eq!(response.json::<serde_json::Value>().await.expect("query failure"),serde_json::json!({"code":"invalid_input"}));
                }
                let response = client.head(format!("{base}/api/v1/roles?size=1")).header("operation-id", OTHER).header("session-id", &administrator.0).header("cookie", &administrator.1).header("origin", "https://example.com").header("x-real-ip", "203.0.113.86").send().await.expect("role list HEAD");
                assert_eq!(response.status(),200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie")); assert!(response.bytes().await.expect("HEAD body").is_empty());
                let response = client.post(format!("{base}/api/v1/roles")).header("operation-id",OTHER).send().await.expect("management write requires trusted source and authentication");
                assert_eq!(response.status(), 400);
                let operation = &document["paths"]["/api/v1/roles"]["get"];
                let parameters = operation["parameters"].as_array().expect("list parameters");
                for (name, minimum) in [("page",1),("size",1)] {
                    let parameter = parameters.iter().find(|parameter| parameter["name"] == name).expect("pagination parameter");
                    assert_eq!(parameter["required"], false); assert_eq!(parameter["schema"]["minimum"], minimum);
                }
                assert_eq!(parameters.iter().find(|parameter| parameter["name"] == "size").expect("size")["schema"]["maximum"],100);
                let collection = resolve(&operation["responses"]["200"]["content"]["application/json"]["schema"]);
                assert_eq!(collection["required"],serde_json::json!(["code","count","roles"]));
                assert_eq!(collection["properties"]["roles"]["items"]["$ref"],"#/components/schemas/role.Role");
                assert!(document["paths"]["/api/v1/roles"]["head"]["responses"]["200"].get("content").is_none());
                let facts=observer.client().query("SELECT change.id,change.role_id,change.name,change.level,change.changed_at,change.operation_id,revision.person_id,revision.id AS revision_id,ARRAY(SELECT capability_id FROM access.entries WHERE change_id=change.id ORDER BY capability_id COLLATE \"C\") AS permissions FROM access.changes change JOIN identity.revisions revision ON revision.id=change.operator_id WHERE change.scope='institution' ORDER BY change.changed_at DESC,change.id DESC",&[]).await.expect("independent history before API");
                let expected:Vec<_>=facts.iter().map(|row|serde_json::json!({"id":row.get::<_,Uuid>("id"),"roleId":row.get::<_,Uuid>("role_id"),"action":"create","name":row.get::<_,String>("name"),"level":row.get::<_,i16>("level"),"permissions":row.get::<_,Vec<String>>("permissions"),"operator":{"personId":row.get::<_,Uuid>("person_id"),"revisionId":row.get::<_,Uuid>("revision_id"),"name":"F*"},"changedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("changed_at"),"operationId":row.get::<_,Uuid>("operation_id")})).collect();
                let response=history(&administrator.0,&administrator.1,"").send().await.expect("system history HTTP");
                assert_eq!(response.status(),200);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("full history shape"),serde_json::json!({"code":"ok","count":2,"changes":expected}));
                assert_eq!(history(&ordinary.0,&ordinary.1,"").send().await.expect("R2 history rejection").status(),403);
                for (query,count,length) in [(format!("?operationId={}",facts[0].get::<_,Uuid>("operation_id")),2,2),(format!("?roleId={}&operationId={}&size=1",facts[0].get::<_,Uuid>("role_id"),facts[0].get::<_,Uuid>("operation_id")),1,1),(format!("?operationId={OTHER}"),0,0),("?page=3&size=1".into(),2,0)] {
                    let response=history(&administrator.0,&administrator.1,&query).send().await.expect("history filters");assert_eq!(response.status(),200);
                    let page=response.json::<serde_json::Value>().await.expect("page");assert_eq!(page["count"],count);assert_eq!(page["changes"].as_array().expect("changes").len(),length);
                }
                assert_eq!(history(&administrator.0,&administrator.1,"?roleId=").send().await.expect("bad filter").status(),400);
                assert_eq!(history(&administrator.0,&administrator.1,"").header("origin","https://wrong.invalid").send().await.expect("history same origin").status(),403);
                let response=client.head(format!("{base}/api/v1/roles/changes")).header("operation-id",OTHER).header("session-id",&administrator.0).header("cookie",&administrator.1).header("origin","https://example.com").header("x-real-ip","203.0.113.87").send().await.expect("history HEAD");
                assert_eq!(response.status(),200);assert!(response.bytes().await.expect("no HEAD body").is_empty());
                let operation=&document["paths"]["/api/v1/roles/changes"]["get"];
                let collection=resolve(&operation["responses"]["200"]["content"]["application/json"]["schema"]);
                assert_eq!(collection["required"],serde_json::json!(["code","count","changes"]));
                let change=resolve(&collection["properties"]["changes"]["items"]);
                assert!(change["allOf"].as_array().expect("history action schema composition").iter().any(|part|part["$ref"]=="#/components/schemas/role.change.Action"));
                assert!(document["paths"]["/api/v1/roles/changes"]["head"]["responses"]["200"].get("content").is_none());
                let response=http_client().post(format!("{base}/api/v1/roles")).header("session-id",&administrator.0).header("cookie",&administrator.1).header("operation-id",Uuid::new_v4().to_string()).header("x-real-ip","203.0.116.3").json(&serde_json::json!({"name":"审阅角色","level":200,"permissions":["institution.roles"]})).send().await.expect("real custom role");
                assert_eq!(response.status(),201);
                let created=response.json::<serde_json::Value>().await.expect("complete custom definition");
                let role_id:Uuid=created["role"]["id"].as_str().expect("role ID").parse().expect("UUID");
                let change_id:Uuid=created["role"]["changeId"].as_str().expect("change ID").parse().expect("UUID");
                replace_role(&base,&administrator,"13800138000",&created["role"],4).await;
                let ordinary_person=current_account(&base,&ordinary).await["personId"].clone();
                let latest=http_client().get(format!("{base}/api/v1/assignments?personId={}&size=1",ordinary_person.as_str().expect("person ID"))).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&administrator.0).header("cookie",&administrator.1).header("x-real-ip","203.0.113.88").send().await.expect("independent assignment history before repeat").json::<serde_json::Value>().await.expect("assignment history");
                assert_eq!(latest["assignments"][0]["role"]["id"],serde_json::json!(role_id),"independent changed role before repeat");
                let response = get(&ordinary.0, &ordinary.1).send().await.expect("same request operation reads current role");
                assert_eq!(response.json::<serde_json::Value>().await.expect("current custom role"),serde_json::json!({"code":"ok","role":{"id":role_id,"kind":"custom","name":"审阅角色","level":200,"permissions":["institution.roles"],"changeId":change_id}}));
                let response = list(&ordinary.0,&ordinary.1,"?size=100").send().await.expect("custom role lists definitions across levels");
                assert_eq!(response.status(), 200);
                let page = response.json::<serde_json::Value>().await.expect("custom-authorized list");
                assert_eq!(page["count"],3); assert_eq!(page["roles"][0]["level"],1000); assert_eq!(page["roles"][1]["id"],serde_json::json!(role_id)); assert_eq!(page["roles"][2]["level"],1);
                let response=http_client().put(format!("{base}/api/v1/roles/{role_id}")).header("session-id",&administrator.0).header("cookie",&administrator.1).header("operation-id",Uuid::new_v4().to_string()).header("x-real-ip","203.0.116.5").json(&serde_json::json!({"changeId":change_id,"name":"当前审阅角色","level":250,"permissions":["institution.roles","institution.accounts"]})).send().await.expect("real definition update");
                assert_eq!(response.status(),200);
                let updated=response.json::<serde_json::Value>().await.expect("updated complete definition");
                let change_id:Uuid=updated["role"]["changeId"].as_str().expect("new change ID").parse().expect("UUID");
                let definitions=list(&administrator.0,&administrator.1,"?size=100").send().await.expect("independent definitions before repeat").json::<serde_json::Value>().await.expect("definitions");
                let definition=definitions["roles"].as_array().expect("roles").iter().find(|role|role["id"]==serde_json::json!(role_id)).expect("updated definition");
                assert_eq!(definition["changeId"],serde_json::json!(change_id),"independent latest definition before repeat");
                let response = get(&ordinary.0,&ordinary.1).send().await.expect("same operation and holder read live definition");
                assert_eq!(response.json::<serde_json::Value>().await.expect("updated custom definition"),serde_json::json!({"code":"ok","role":{"id":role_id,"kind":"custom","name":"当前审阅角色","level":250,"permissions":["institution.accounts","institution.roles"],"changeId":change_id}}));
                let owner = connections(&server, &name, "postgres").recover().await.expect("isolated query fault injector");
                owner.client().batch_execute("REVOKE SELECT ON access.holders FROM clerk").await.expect("real decisive query failure after valid authentication");
                let response = get(&ordinary.0, &ordinary.1).send().await.expect("current role query failure");
                assert_eq!(response.status(),503);
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe current failure"),serde_json::json!({"code":"dependency_unavailable"}));
                let response = list(&ordinary.0, &ordinary.1, "").send().await.expect("query failure");
                assert_eq!(response.status(),503); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe failure"),serde_json::json!({"code":"dependency_unavailable"}));
                let response=history(&ordinary.0,&ordinary.1,"").send().await.expect("history query failure");assert_eq!(response.status(),503);
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe history failure"),serde_json::json!({"code":"dependency_unavailable"}));
                owner.client().batch_execute("GRANT SELECT ON access.holders TO clerk").await.expect("restore isolated grants");
                assert_eq!(get(&ordinary.0,&ordinary.1).send().await.expect("fresh successful retry").status(),200);
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.revocations",&[]).await.expect("reads do not revoke").get::<_,i64>(0),0);
                let response = client.delete(format!("{base}/api/v1/sessions")).header("operation-id",Uuid::new_v4().to_string()).header("session-id",&ordinary.0).header("cookie",&ordinary.1).header("origin","https://example.com").header("x-real-ip","203.0.113.85").send().await.expect("actual logout after successful role read");
                assert_eq!(response.status(),200);
                assert!(observer.client().query_one("SELECT EXISTS (SELECT 1 FROM identity.revocations WHERE id=$1::text::uuid)", &[&ordinary.0]).await.expect("independent revoked session").get::<_,bool>(0));
                let response = get(&ordinary.0,&ordinary.1).send().await.expect("role read after logout");
                assert_eq!(response.status(),401); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("invalid actor"),serde_json::json!({"code":"authentication_invalid"}));
                assert_eq!(list(&ordinary.0,&ordinary.1, "").send().await.expect("list after real logout").status(),401);
                let changes: i64 = observer.client().query_one("SELECT count(*) FROM access.changes", &[]).await.expect("original history before integrity fault").get(0);
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated holder damage");
                assert_eq!(owner.client().execute("DELETE FROM access.holders WHERE id=(SELECT account_id FROM identity.sessions WHERE id=$1::text::uuid)", &[&administrator.0]).await.expect("remove necessary current relationship"), 1);
                owner.client().batch_execute("COMMIT").await.expect("independent damage visible");
                let response = http_client().get(format!("{base}/api/v1/accounts/current/role")).header("session-id", &administrator.0).header("cookie", &administrator.1).header("operation-id", "00000000-0000-4000-a000-000000000001").send().await.expect("damaged current role");
                assert_eq!(response.status(), 503);
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe integrity failure"), serde_json::json!({"code":"dependency_unavailable"}));
                let response = client.get(format!("{base}/health/ready")).header("operation-id", "00000000-0000-4000-a000-000000000002").send().await.expect("public readiness after damage");
                assert_eq!(response.status(), 503);
                assert_eq!(response.json::<serde_json::Value>().await.expect("closed readiness"), serde_json::json!({"code":"dependency_unavailable"}));
                let response = http_client().post(format!("{base}/api/v1/roles")).header("session-id", &administrator.0).header("cookie", &administrator.1).header("operation-id", "00000000-0000-4000-a000-000000000003").json(&serde_json::json!({"name":"After read damage","level":20,"permissions":[]})).send().await.expect("new HTTP work after damage");
                assert_eq!(response.status(), 503); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("closed business admission"), serde_json::json!({"code":"dependency_unavailable"}));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes", &[]).await.expect("no admitted write or partial repair").get::<_, i64>(0), changes);
                owner.terminate().await.expect("owner ended"); observer.terminate().await.expect("observer ended");
                stop.send(()).expect("stop"); let (application,result)=task.await.expect("server"); result.expect("drained"); drop(application);
                assert_eq!(supplier.calls.load(Ordering::SeqCst),2); assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err()); sms.stop().await;
            }).await.expect("role HTTP budget");
        });
    });
}

#[test]
fn workforce_role_history_exposes_only_authorized_permanent_http_facts() {
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
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.163.{address}"))
            };
            let path = "/api/v1/workforce/roles/changes";
            for actor in [&manager, &member] {
                let response = request(reqwest::Method::GET, path, actor, Uuid::new_v4()).send().await.expect("no domain permission");
                assert_eq!(response.status(), 403);
                assert_eq!(response.json::<serde_json::Value>().await.expect("denial"), serde_json::json!({"code":"permission_denied"}));
            }
            let response = request(reqwest::Method::POST, "/api/v1/workforce/roles", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","name":"History reader","level":200,"permissions":["workforce.roles"]})).send().await.expect("reader role");
            assert_eq!(response.status(), 201);
            let role = response.json::<serde_json::Value>().await.expect("role")["role"].clone();
            let response = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate");
            assert_eq!(response.status(), 200);
            let candidate = response.json::<serde_json::Value>().await.expect("confirmation");
            let response = request(reqwest::Method::POST, "/api/v1/workforce/grants", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","target":{
                    "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                    "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
                },"role":{"id":role["id"],"changeId":role["changeId"]}})).send().await.expect("real membership");
            assert_eq!(response.status(), 201);
            let response = request(reqwest::Method::POST, "/api/v1/workforce/roles", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","name":"Higher","level":700,"permissions":["workforce.numbers"]})).send().await.expect("higher role");
            assert_eq!(response.status(), 201);
            let original = response.json::<serde_json::Value>().await.expect("created")["role"].clone();
            let target = format!("/api/v1/workforce/roles/{}", original["id"].as_str().expect("role ID"));
            let response = request(reqwest::Method::PUT, &target, &manager, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","changeId":original["changeId"],"name":"Higher changed","level":701,"permissions":[]})).send().await.expect("modification");
            assert_eq!(response.status(), 200);
            let updated = response.json::<serde_json::Value>().await.expect("updated")["role"].clone();
            let operation = Uuid::new_v4();
            let response = request(reqwest::Method::DELETE, &target, &manager, operation)
                .json(&serde_json::json!({"entry":"institution","changeId":updated["changeId"]})).send().await.expect("deletion");
            assert_eq!(response.status(), 200);
            let deleted = response.json::<serde_json::Value>().await.expect("deleted")["change"].clone();
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
            let facts = observer.client().query(r#"SELECT change.id,change.role_id,change.action,change.name,change.level,change.changed_at,change.operation_id,revision.person_id,revision.id AS revision_id,ARRAY(SELECT capability_id FROM access.entries WHERE change_id=change.id ORDER BY capability_id COLLATE "C") AS permissions FROM access.changes change JOIN identity.revisions revision ON revision.id=change.operator_id WHERE change.scope='workforce' ORDER BY change.changed_at DESC,change.id DESC"#, &[]).await.expect("independent permanent history before reads");
            let expected: Vec<_> = facts.iter().map(|row| {
                let action: &str = row.get("action");
                let mut value = serde_json::json!({"id":row.get::<_,Uuid>("id"),"roleId":row.get::<_,Uuid>("role_id"),"action":action,"name":row.get::<_,String>("name"),"level":row.get::<_,i16>("level"),
                    "operator":{"personId":row.get::<_,Uuid>("person_id"),"revisionId":row.get::<_,Uuid>("revision_id"),"name":"F*"},"changedAt":row.get::<_,chrono::DateTime<chrono::Utc>>("changed_at"),"operationId":row.get::<_,Uuid>("operation_id")});
                if action != "delete" { value["permissions"] = serde_json::json!(row.get::<_,Vec<String>>("permissions")); }
                value
            }).collect();
            assert_eq!(expected.len(), 6);
            assert_eq!(expected[0], deleted);
            let read_id = Uuid::new_v4();
            for (query, count, changes) in [
                (String::new(), 6, expected.clone()),
                ("?page=2&size=1".into(), 6, vec![expected[1].clone()]),
                ("?page=9223372036854775807&size=1".into(), 6, vec![]),
                (format!("?roleId={}", original["id"].as_str().expect("id")), 3, expected[..3].to_vec()),
                (format!("?operationId={operation}"), 1, vec![deleted.clone()]),
                (format!("?roleId={}&operationId={operation}", original["id"].as_str().expect("id")), 1, vec![deleted.clone()]),
                (format!("?roleId={}&operationId={operation}", role["id"].as_str().expect("id")), 0, vec![]),
            ] {
                let response = request(reqwest::Method::GET, &format!("{path}{query}"), &member, read_id).send().await.expect("history page");
                assert_eq!(response.status(), 200, "{query}");
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("complete history shape"), serde_json::json!({"code":"ok","count":count,"changes":changes}));
            }
            for query in ["page=0", "size=101", "page=1&page=2", "page=9223372036854775807&size=2", "entry=domain", "roleId=", "roleId=invalid", "operationId=", "operationId=invalid", "roleId=00000000-0000-4000-a000-000000000001&roleId=00000000-0000-4000-a000-000000000002"] {
                let response = request(reqwest::Method::GET, &format!("{path}?{query}"), &member, read_id).send().await.expect("invalid filter");
                assert_eq!(response.status(), 400, "{query}");
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe filter failure"), serde_json::json!({"code":"invalid_input"}));
            }
            let response = request(reqwest::Method::HEAD, path, &member, read_id).send().await.expect("HEAD history");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            assert!(response.bytes().await.expect("HEAD bytes").is_empty());
            assert_eq!(request(reqwest::Method::HEAD, path, &manager, read_id).send().await.expect("HEAD authority").status(), 403);
            let response = client.get(format!("{base}{path}")).header("operation-id", read_id.to_string()).header("x-real-ip", "203.0.163.250").send().await.expect("missing identity");
            assert_eq!(response.status(), 401);
            let response = request(reqwest::Method::GET, path, &member, read_id).header("sec-fetch-site", "cross-site").send().await.expect("cross-site rejected");
            assert_eq!(response.status(), 403);
            let owner = connections(&server, &name, "postgres").recover().await.expect("isolated fault injector");
            owner.client().batch_execute("REVOKE SELECT ON identity.revisions FROM clerk").await.expect("read failure");
            let response = request(reqwest::Method::GET, path, &member, read_id).send().await.expect("dependency failure");
            assert_eq!(response.status(), 503);
            assert_eq!(response.json::<serde_json::Value>().await.expect("safe error"), serde_json::json!({"code":"dependency_unavailable"}));
            owner.client().batch_execute("GRANT SELECT ON identity.revisions TO clerk").await.expect("restore isolated read grant");
            assert_eq!(request(reqwest::Method::GET, path, &member, read_id).send().await.expect("fresh read").status(), 200);
            assert_eq!(request(reqwest::Method::GET, path, &member, Uuid::new_v4()).send().await.expect("history after reads").json::<serde_json::Value>().await.expect("history")["count"], 6, "reads preserve facts");
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let route = &document["paths"][path]["get"];
            assert_eq!(route["operationId"], "listWorkforceRoleChanges");
            assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/role.change.List");
            assert!(route["parameters"].as_array().expect("parameters").iter().all(|parameter| parameter["name"] != "Origin" && parameter["name"] != "entry"));
            for field in ["roleId", "operationId"] {
                let parameter = route["parameters"].as_array().expect("parameters").iter().find(|parameter| parameter["name"] == field).expect("filter parameter");
                assert_eq!(parameter["required"], false); assert_eq!(parameter["schema"]["format"], "uuid");
            }
            assert!(document["paths"][path]["head"]["responses"]["200"].get("content").is_none());
            owner.terminate().await.expect("owner ended"); observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application, ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
            assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded HTTP history acceptance");
    });
}

#[test]
fn workforce_roles_use_typed_http_authority_and_paginated_admission() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(40),async {
            let name=database(&server).await;
            let supplier=Supplier::echoing().await;let mut sms=fixture::supplier::Supplier::start().await;
            let application=Application::start(connections(&server,&name,"clerk"),keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("verified startup");
            let (base,stop,task)=running(application).await;
            let manager=issued_session_for(&base,&mut sms,"13900000000").await;
            let member=issued_session_for(&base,&mut sms,"13800138000").await;
            let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source=std::cell::Cell::new(1_u8);
            let request=|path:&str,actor:&(String,String),operation:Uuid| {
                let address=source.get();source.set(address+1);
                client.post(format!("{base}{path}")).header("operation-id",operation.to_string()).header("session-id",&actor.0).header("cookie",&actor.1).header("x-real-ip",format!("203.0.143.{address}"))
            };
            let list = |method: reqwest::Method, path: &str, actor: &(String, String)| {
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", "00000000-0000-4000-a000-000000000001").header("session-id", &actor.0).header("cookie", &actor.1).header("x-real-ip", format!("203.0.143.{address}"))
            };
            for (request, status, code) in [
                (list(reqwest::Method::GET, "/api/v1/workforce/roles", &manager), 403, "permission_denied"),
                (list(reqwest::Method::GET, "/api/v1/workforce/roles", &manager).header("origin", "https://wrong.invalid"), 403, "permission_denied"),
                (list(reqwest::Method::GET, "/api/v1/workforce/roles", &manager).header("origin", "https://example.com").header("sec-fetch-site", "cross-site"), 403, "permission_denied"),
                (list(reqwest::Method::GET, "/api/v1/workforce/roles", &member).header("sec-fetch-site", "same-origin"), 403, "permission_denied"),
                (client.get(format!("{base}/api/v1/workforce/roles")).header("operation-id", Uuid::new_v4().to_string()).header("x-real-ip", "203.0.143.200").header("sec-fetch-site", "same-origin"), 401, "authentication_invalid"),
            ] {
                let response = request.send().await.expect("role-list request boundary");
                assert_eq!(response.status(), status);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("bounded failure"), serde_json::json!({"code":code}));
            }
            for query in ["page=0", "size=101", "page=1&page=2", "entry=domain", "page=9223372036854775807&size=2"] {
                let response = list(reqwest::Method::GET, &format!("/api/v1/workforce/roles?{query}"), &manager).header("sec-fetch-site", "same-origin").send().await.expect("invalid role-list query");
                assert_eq!(response.status(), 400);
                assert_eq!(response.json::<serde_json::Value>().await.expect("query rejection"), serde_json::json!({"code":"invalid_input"}));
            }
            let response = list(reqwest::Method::GET, "/api/v1/workforce/roles", &manager).header("sec-fetch-site", "same-origin").send().await.expect("institution reader without membership");
            assert_eq!(response.status(), 200);
            let initial = response.json::<serde_json::Value>().await.expect("initial definitions");
            assert_eq!(initial["count"], 2);
            assert_eq!(initial["roles"][0]["kind"], "full");
            assert_eq!(initial["roles"][1]["kind"], "default");
            let body=serde_json::json!({"entry":"institution","name":" Coordinators ","level":300,"permissions":["workforce.roles","workforce.admission"]});
            for (builder,status,code) in [
                (request("/api/v1/workforce/roles",&manager,Uuid::new_v4()).json(&body),403,"permission_denied"),
                (request("/api/v1/workforce/roles",&manager,Uuid::new_v4()).header("origin","https://wrong.invalid").json(&body),403,"permission_denied"),
                (request("/api/v1/workforce/roles",&member,Uuid::new_v4()).header("origin","https://example.com").json(&body),403,"permission_denied"),
                (request("/api/v1/workforce/roles",&member,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","name":"Invalid","level":200,"permissions":["institution.roles"]})),422,"invalid_input"),
                (request("/api/v1/workforce/roles",&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"name":"Missing entry","level":200,"permissions":[]})),422,"invalid_input"),
                (request("/api/v1/workforce/roles",&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"domain","name":"No membership","level":200,"permissions":[]})),403,"permission_denied"),
                (request("/api/v1/workforce/roles",&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","name":"System","level":1000,"permissions":[]})),403,"permission_denied"),
            ] {
                let response=builder.send().await.expect("actual boundary");assert_eq!(response.status(),status);
                assert_eq!(response.json::<serde_json::Value>().await.expect("safe result"),serde_json::json!({"code":code}));
            }
            let operation=Uuid::new_v4();
            let response=request("/api/v1/workforce/roles",&manager,operation).header("origin","https://example.com").json(&body).send().await.expect("institutional creation");
            assert_eq!(response.status(),201);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
            let original=response.json::<serde_json::Value>().await.expect("complete role");assert_eq!(original["code"],"ok");
            let role=&original["role"];assert_eq!(role["name"],"Coordinators");assert_eq!(role["kind"],"custom");assert_eq!(role.as_object().expect("role").len(),6);
            assert_eq!(role["permissions"],serde_json::json!(["workforce.admission","workforce.roles"]));
            let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
            assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.roles role JOIN access.changes change ON change.id=role.change_id AND change.role_id=role.id WHERE change.operation_id=$1 AND change.scope='workforce' AND change.entry='institution' AND role.name='Coordinators' AND role.level=300 AND (SELECT count(*) FROM access.entries WHERE change_id=change.id)=2 AND (SELECT count(*) FROM access.permissions WHERE role_id=role.id)=2)",&[&operation]).await.expect("complete persisted facts before replay").get::<_,bool>(0));
            let replay=request("/api/v1/workforce/roles",&manager,operation).header("origin","https://example.com").json(&body).send().await.expect("same request");assert_eq!(replay.status(),201);assert_eq!(replay.json::<serde_json::Value>().await.expect("original"),original);
            let mut changed=body.clone();changed["level"]=serde_json::json!(299);
            let mismatch=request("/api/v1/workforce/roles",&manager,operation).header("origin","https://example.com").json(&changed).send().await.expect("changed request");assert_eq!(mismatch.status(),409);assert_eq!(mismatch.json::<serde_json::Value>().await.expect("mismatch"),serde_json::json!({"code":"request_mismatch"}));
            let duplicate=request("/api/v1/workforce/roles",&manager,Uuid::new_v4()).header("origin","https://example.com").json(&body).send().await.expect("new key same name");assert_eq!(duplicate.status(),409);assert_eq!(duplicate.json::<serde_json::Value>().await.expect("name conflict"),serde_json::json!({"code":"role_name_taken"}));
            let search=serde_json::json!({"entry":"institution","mobile":"13800138000"});
            for query in ["?size=0","?entry=domain"] {
                let response=request(&format!("/api/v1/workforce/grants/candidates{query}"),&manager,Uuid::new_v4()).header("origin","https://example.com").json(&search).send().await.expect("invalid pagination");assert_eq!(response.status(),400);assert_eq!(response.json::<serde_json::Value>().await.expect("input rejection"),serde_json::json!({"code":"invalid_input"}));
            }
            let response=request("/api/v1/workforce/grants/candidates?page=2&size=1",&manager,Uuid::new_v4()).header("origin","https://example.com").json(&search).send().await.expect("custom role page");assert_eq!(response.status(),200);
            let candidate=response.json::<serde_json::Value>().await.expect("candidate");assert_eq!(candidate["count"],3);assert_eq!(candidate["roles"],serde_json::json!([role]));
            let response=request("/api/v1/workforce/grants/candidates?page=4&size=1",&manager,Uuid::new_v4()).header("origin","https://example.com").json(&search).send().await.expect("empty page");assert_eq!(response.status(),200);
            let empty=response.json::<serde_json::Value>().await.expect("empty candidate");assert_eq!(empty["count"],3);assert_eq!(empty["roles"],serde_json::json!([]));
            let admission=serde_json::json!({"entry":"institution","target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]},"role":{"id":role["id"],"changeId":role["changeId"]}});
            let response=request("/api/v1/workforce/grants",&manager,Uuid::new_v4()).header("origin","https://example.com").json(&admission).send().await.expect("custom role admission");assert_eq!(response.status(),201);assert_eq!(response.json::<serde_json::Value>().await.expect("assignment")["assignment"]["role"],*role);
            for actor in [&manager, &member] {
                let response = list(reqwest::Method::GET, "/api/v1/workforce/roles?page=2&size=1", actor).header("origin", "https://example.com").send().await.expect("either role-list authority");
                assert_eq!(response.status(), 200);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("complete list shape"), serde_json::json!({"code":"ok","count":3,"roles":[role]}));
                let response = list(reqwest::Method::GET, "/api/v1/workforce/roles?page=4&size=1", actor).header("sec-fetch-site", "same-origin").send().await.expect("empty page");
                assert_eq!(response.status(), 200);
                assert_eq!(response.json::<serde_json::Value>().await.expect("empty list shape"), serde_json::json!({"code":"ok","count":3,"roles":[]}));
                let response = list(reqwest::Method::HEAD, "/api/v1/workforce/roles", actor).header("sec-fetch-site", "same-origin").send().await.expect("HEAD uses the same authority");
                assert_eq!(response.status(), 200);
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert!(response.bytes().await.expect("HEAD body").is_empty());
            }
            let body=serde_json::json!({"entry":"domain","name":"Lower role","level":200,"permissions":["workforce.admission"]});
            let response=request("/api/v1/workforce/roles",&member,Uuid::new_v4()).header("origin","https://example.com").json(&body).send().await.expect("qualified domain creation");assert_eq!(response.status(),201);
            let response = list(reqwest::Method::GET, "/api/v1/workforce/roles", &member).header("sec-fetch-site", "same-origin").send().await.expect("same read identifier obtains current definitions");
            assert_eq!(response.status(), 200);
            let current = response.json::<serde_json::Value>().await.expect("current role list");
            assert_eq!(current["count"], 4);
            assert_eq!(current["roles"][0]["level"], 1000, "domain readers can see higher roles");
            let mut expansion=body;expansion["permissions"]=serde_json::json!(["workforce.numbers"]);
            let response=request("/api/v1/workforce/roles",&member,Uuid::new_v4()).header("origin","https://example.com").json(&expansion).send().await.expect("domain expansion rejected");assert_eq!(response.status(),403);
            let schema=client.get(format!("{base}/openapi/v1.json")).send().await.expect("schema").json::<serde_json::Value>().await.expect("OpenAPI");
            let route=&schema["paths"]["/api/v1/workforce/roles"]["post"];
            for name in ["Operation-Id","Session-Id","Origin"] {assert_eq!(route["parameters"].as_array().expect("headers").iter().find(|item|item["name"]==name).expect("required header")["required"],true);}
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/role.workforce.Input");
            let permissions=schema["components"]["schemas"]["role.workforce.Permissions"]["items"]["enum"].as_array().expect("scoped permission codes");assert_eq!(permissions.len(),11);assert!(permissions.iter().all(|code|code.as_str().expect("code").starts_with("workforce.")));
            for name in ["page","size"] {assert_eq!(schema["paths"]["/api/v1/workforce/grants/candidates"]["post"]["parameters"].as_array().expect("query").iter().find(|item|item["name"]==name).expect("optional pagination")["required"],false);}
            let route = &schema["paths"]["/api/v1/workforce/roles"]["get"];
            assert_eq!(route["operationId"], "listWorkforceRoles");
            let parameters = route["parameters"].as_array().expect("role-list parameters");
            for name in ["Operation-Id", "Session-Id"] { assert_eq!(parameters.iter().find(|parameter| parameter["name"] == name).expect("required header")["required"], true); }
            for name in ["page", "size"] { assert_eq!(parameters.iter().find(|parameter| parameter["name"] == name).expect("optional pagination")["required"], false); }
            assert!(!parameters.iter().any(|parameter| parameter["name"] == "Origin" || parameter["name"] == "entry"));
            assert_eq!(route["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/role.List");
            observer.terminate().await.expect("observer ended");stop.send(()).expect("stop");let(application,ended)=task.await.expect("process");ended.expect("drained");drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err(),"no supplier call through replay and drain");sms.stop().await;
        }).await.expect("HTTP Workforce role deadline");
    });
}

#[test]
fn workforce_role_modification_uses_typed_input_and_changes_current_authority() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(45),async {
            let name=database(&server).await;
            let supplier=Supplier::echoing().await;let mut sms=fixture::supplier::Supplier::start().await;
            let application=Application::start(connections(&server,&name,"clerk"),keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("verified startup");
            let (base,stop,task)=running(application).await;
            let institution=issued_session_for(&base,&mut sms,"13900000000").await;
            let manager=issued_session_for(&base,&mut sms,"13800138000").await;
            let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source=std::cell::Cell::new(1_u8);
            let request=|method:reqwest::Method,path:&str,actor:&(String,String),operation:Uuid| {
                let address=source.get();source.set(address+1);
                client.request(method,format!("{base}{path}")).header("operation-id",operation.to_string()).header("session-id",&actor.0).header("cookie",&actor.1).header("x-real-ip",format!("203.0.143.{address}"))
            };
            let response=request(reqwest::Method::POST,"/api/v1/workforce/roles",&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","name":"Maintainer","level":300,"permissions":["workforce.roles"]})).send().await.expect("authority creation");assert_eq!(response.status(),201);
            let authority=response.json::<serde_json::Value>().await.expect("authority")["role"].clone();
            let response=request(reqwest::Method::POST,"/api/v1/workforce/roles",&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","name":"Target","level":200,"permissions":["workforce.numbers"]})).send().await.expect("target creation");assert_eq!(response.status(),201);
            let target=response.json::<serde_json::Value>().await.expect("target")["role"].clone();let path=format!("/api/v1/workforce/roles/{}",target["id"].as_str().expect("id"));
            let body=serde_json::json!({"entry":"domain","changeId":target["changeId"],"name":"Updated","level":201,"permissions":["workforce.roles","workforce.numbers"]});
            for (builder,status,code) in [
                (request(reqwest::Method::PUT,&path,&manager,Uuid::new_v4()).json(&body),403,"permission_denied"),
                (request(reqwest::Method::PUT,&path,&manager,Uuid::new_v4()).header("origin","https://wrong.invalid").json(&body),403,"permission_denied"),
                (request(reqwest::Method::PUT,&path,&manager,Uuid::new_v4()).header("origin","https://example.com").header("sec-fetch-site","cross-site").json(&body),403,"permission_denied"),
                (request(reqwest::Method::PUT,"/api/v1/workforce/roles/invalid",&manager,Uuid::new_v4()).header("origin","https://example.com").json(&body),400,"invalid_input"),
                (request(reqwest::Method::PUT,&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&body),403,"permission_denied"),
                (request(reqwest::Method::PUT,&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"domain","changeId":"invalid","name":"Invalid","level":200,"permissions":[]})),422,"invalid_input"),
                (request(reqwest::Method::PUT,&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"domain","changeId":target["changeId"],"name":"Invalid","level":200,"permissions":["institution.roles"]})),422,"invalid_input"),
                (request(reqwest::Method::PUT,&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"changeId":target["changeId"]})),422,"invalid_input"),
                (request(reqwest::Method::PUT,&path,&manager,Uuid::new_v4()).header("origin","https://example.com").body("x".repeat(8193)).header("content-type","application/json"),413,"invalid_input"),
            ] {
                let response=builder.send().await.expect("input boundary");assert_eq!(response.status(),status);assert_eq!(response.json::<serde_json::Value>().await.expect("rejection"),serde_json::json!({"code":code}));
            }
            let response=request(reqwest::Method::POST,"/api/v1/workforce/grants/candidates",&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate");assert_eq!(response.status(),200);
            let candidate=response.json::<serde_json::Value>().await.expect("candidate");
            let admission=serde_json::json!({"entry":"institution","target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]},"role":{"id":authority["id"],"changeId":authority["changeId"]}});
            let response=request(reqwest::Method::POST,"/api/v1/workforce/grants",&institution,Uuid::new_v4()).header("origin","https://example.com").json(&admission).send().await.expect("real membership");assert_eq!(response.status(),201);
            let original_id=Uuid::new_v4();let response=request(reqwest::Method::PUT,&path,&manager,original_id).header("origin","https://example.com").json(&body).send().await.expect("domain update");assert_eq!(response.status(),200);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
            let original=response.json::<serde_json::Value>().await.expect("updated role");assert_eq!(original["role"]["id"],target["id"]);assert_ne!(original["role"]["changeId"],target["changeId"]);assert_eq!(original["role"]["permissions"],serde_json::json!(["workforce.numbers","workforce.roles"]));
            let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
            let id:Uuid=target["id"].as_str().expect("id").parse().expect("UUID");
            assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.roles role JOIN access.changes change ON change.id=role.change_id WHERE role.id=$1 AND change.operation_id=$2 AND change.action='update' AND change.entry='domain' AND role.name='Updated' AND role.level=201 AND (SELECT count(*) FROM access.entries WHERE change_id=change.id)=2 AND (SELECT count(*) FROM access.permissions WHERE role_id=role.id)=2)",&[&id,&original_id]).await.expect("independent complete graph before replay").get::<_,bool>(0));
            let response=request(reqwest::Method::PUT,&path,&manager,original_id).header("origin","https://example.com").json(&body).send().await.expect("original");assert_eq!(response.status(),200);assert_eq!(response.json::<serde_json::Value>().await.expect("same receipt"),original);
            let response=request(reqwest::Method::PUT,&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&body).send().await.expect("old confirmation");assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("changed"),serde_json::json!({"code":"role_changed"}));
            let mut changed=body.clone();changed["changeId"]=original["role"]["changeId"].clone();
            let response=request(reqwest::Method::PUT,&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&changed).send().await.expect("unchanged");assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("no empty change"),serde_json::json!({"code":"state_conflict"}));
            let authority_path=format!("/api/v1/workforce/roles/{}",authority["id"].as_str().expect("id"));let cleared=serde_json::json!({"entry":"institution","changeId":authority["changeId"],"name":"No maintenance","level":301,"permissions":[]});
            let response=request(reqwest::Method::PUT,&authority_path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&cleared).send().await.expect("institution update with lower holder");assert_eq!(response.status(),200);let current=response.json::<serde_json::Value>().await.expect("new current role")["role"].clone();
            let response=request(reqwest::Method::GET,"/api/v1/workforce/current/role",&manager,Uuid::new_v4()).header("origin","https://example.com").send().await.expect("live role read");assert_eq!(response.status(),200);assert_eq!(response.json::<serde_json::Value>().await.expect("live definition")["role"],current);
            let response=request(reqwest::Method::PUT,&path,&manager,original_id).header("origin","https://example.com").json(&body).send().await.expect("basic authority precedes original");assert_eq!(response.status(),403);
            let schema=client.get(format!("{base}/openapi/v1.json")).send().await.expect("schema").json::<serde_json::Value>().await.expect("OpenAPI");let route=&schema["paths"]["/api/v1/workforce/roles/{id}"]["put"];
            for name in ["id","Operation-Id","Session-Id","Origin"] {assert_eq!(route["parameters"].as_array().expect("parameters").iter().find(|item|item["name"]==name).expect("parameter")["required"],true);}
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/role.workforce.Update");
            let required=schema["components"]["schemas"]["role.workforce.Update"]["required"].as_array().expect("required fields");for field in ["entry","changeId","name","level","permissions"]{assert!(required.contains(&serde_json::json!(field)));}
            observer.terminate().await.expect("observer ended");stop.send(()).expect("stop");let(application,ended)=task.await.expect("process");ended.expect("drained");drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err());sms.stop().await;
        }).await.expect("bounded HTTP modification");
    });
}

#[test]
fn workforce_role_deletion_uses_typed_confirmation_and_permanent_http_results() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(45),async {
            let name=database(&server).await;
            let supplier=Supplier::echoing().await;let mut sms=fixture::supplier::Supplier::start().await;
            let application=Application::start(connections(&server,&name,"clerk"),keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("verified startup");
            let (base,stop,task)=running(application).await;
            let institution=issued_session_for(&base,&mut sms,"13900000000").await;
            let manager=issued_session_for(&base,&mut sms,"13800138000").await;
            let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source=std::cell::Cell::new(1_u8);
            let request=|method:reqwest::Method,path:&str,actor:&(String,String),operation:Uuid| {
                let address=source.get();source.set(address+1);
                client.request(method,format!("{base}{path}")).header("operation-id",operation.to_string()).header("session-id",&actor.0).header("cookie",&actor.1).header("x-real-ip",format!("203.0.142.{address}"))
            };
            let body=serde_json::json!({"entry":"institution","name":"Maintainer","level":300,"permissions":["workforce.roles"]});
            let response=request(reqwest::Method::POST,"/api/v1/workforce/roles",&institution,Uuid::new_v4()).header("origin","https://example.com").json(&body).send().await.expect("authority creation");assert_eq!(response.status(),201);
            let authority=response.json::<serde_json::Value>().await.expect("authority")["role"].clone();
            let body=serde_json::json!({"entry":"institution","name":"Unused","level":200,"permissions":["workforce.numbers"]});
            let response=request(reqwest::Method::POST,"/api/v1/workforce/roles",&institution,Uuid::new_v4()).header("origin","https://example.com").json(&body).send().await.expect("target creation");assert_eq!(response.status(),201);
            let role=response.json::<serde_json::Value>().await.expect("target")["role"].clone();
            let path=format!("/api/v1/workforce/roles/{}",role["id"].as_str().expect("id"));
            let deletion=serde_json::json!({"entry":"domain","changeId":role["changeId"]});
            for (builder,status,code) in [
                (request(reqwest::Method::DELETE,&path,&institution,Uuid::new_v4()).json(&deletion),403,"permission_denied"),
                (request(reqwest::Method::DELETE,&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&deletion),403,"permission_denied"),
                (request(reqwest::Method::DELETE,&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"domain","changeId":"invalid"})),422,"invalid_input"),
                (request(reqwest::Method::DELETE,&path,&manager,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"changeId":role["changeId"]})),422,"invalid_input"),
            ] {
                let response=builder.send().await.expect("input and authority boundary");assert_eq!(response.status(),status);assert_eq!(response.json::<serde_json::Value>().await.expect("safe rejection"),serde_json::json!({"code":code}));
            }
            let response=request(reqwest::Method::POST,"/api/v1/workforce/grants/candidates",&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate");assert_eq!(response.status(),200);
            let candidate=response.json::<serde_json::Value>().await.expect("confirmation");
            let admission=serde_json::json!({"entry":"institution","target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]},"role":{"id":authority["id"],"changeId":authority["changeId"]}});
            let response=request(reqwest::Method::POST,"/api/v1/workforce/grants",&institution,Uuid::new_v4()).header("origin","https://example.com").json(&admission).send().await.expect("actual membership");assert_eq!(response.status(),201);
            let occupied=format!("/api/v1/workforce/roles/{}",authority["id"].as_str().expect("id"));
            let response=request(reqwest::Method::DELETE,&occupied,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","changeId":authority["changeId"]})).send().await.expect("occupied role");assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("in use"),serde_json::json!({"code":"role_in_use"}));
            let operation=Uuid::new_v4();
            let response=request(reqwest::Method::DELETE,&path,&manager,operation).header("origin","https://example.com").json(&deletion).send().await.expect("qualified domain deletion");
            assert_eq!(response.status(),200);assert_eq!(response.headers()["cache-control"],"no-store");assert!(!response.headers().contains_key("set-cookie"));
            let original=response.json::<serde_json::Value>().await.expect("terminal result");assert_eq!(original["code"],"ok");
            let change=&original["change"];assert_eq!(change["roleId"],role["id"]);assert_eq!(change["action"],"delete");assert!(change.get("permissions").is_none());assert_eq!(change["operator"].as_object().expect("masked fixed operator").len(),3);
            let observer=connections(&server,&name,"clerk").recover().await.expect("independent observer");
            let id:Uuid=role["id"].as_str().expect("id").parse().expect("UUID");
            assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.roles role JOIN access.changes change ON change.id=role.change_id AND change.role_id=role.id AND change.sequence=role.sequence WHERE role.id=$1 AND NOT role.active AND cardinality(role.permissions)=0 AND change.operation_id=$2 AND change.entry='domain' AND change.action='delete') AND NOT EXISTS(SELECT 1 FROM access.permissions WHERE role_id=$1) AND NOT EXISTS(SELECT 1 FROM access.entries entry JOIN access.changes change ON change.id=entry.change_id WHERE change.role_id=$1 AND change.action='delete')",&[&id,&operation]).await.expect("independent committed tombstone before replay").get::<_,bool>(0));
            let response=request(reqwest::Method::POST,"/api/v1/workforce/roles",&institution,Uuid::new_v4()).header("origin","https://example.com").json(&body).send().await.expect("name reuse");assert_eq!(response.status(),201);let replacement=response.json::<serde_json::Value>().await.expect("new role")["role"].clone();assert_ne!(replacement["id"],role["id"]);
            let response=request(reqwest::Method::DELETE,&path,&manager,operation).header("origin","https://example.com").json(&deletion).send().await.expect("replay");assert_eq!(response.status(),200);assert_eq!(response.json::<serde_json::Value>().await.expect("same original"),original);
            let response=request(reqwest::Method::DELETE,&path,&manager,operation).header("origin","https://example.com").json(&serde_json::json!({"entry":"domain","changeId":replacement["changeId"]})).send().await.expect("changed confirmation");assert_eq!(response.status(),409);assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch"),serde_json::json!({"code":"request_mismatch"}));
            let path=format!("/api/v1/workforce/roles/{}",replacement["id"].as_str().expect("id"));
            let response=request(reqwest::Method::DELETE,&path,&institution,Uuid::new_v4()).header("origin","https://example.com").json(&serde_json::json!({"entry":"institution","changeId":replacement["changeId"]})).send().await.expect("institution deletion without membership");assert_eq!(response.status(),200);
            let schema=client.get(format!("{base}/openapi/v1.json")).send().await.expect("schema").json::<serde_json::Value>().await.expect("OpenAPI");let route=&schema["paths"]["/api/v1/workforce/roles/{id}"]["delete"];
            for name in ["id","Operation-Id","Session-Id","Origin"] {assert_eq!(route["parameters"].as_array().expect("parameters").iter().find(|item|item["name"]==name).expect("required parameter")["required"],true);}
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"],"#/components/schemas/role.workforce.Delete");
            observer.terminate().await.expect("observer ended");stop.send(()).expect("stop");let(application,ended)=task.await.expect("process");ended.expect("drained");drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst),2);assert!(timeout(Duration::from_millis(30),sms.request()).await.is_err());sms.stop().await;
        }).await.expect("bounded HTTP deletion");
    });
}

pub(crate) async fn replace_role(
    base: &str,
    manager: &(String, String),
    mobile: &str,
    role: &serde_json::Value,
    source: u8,
) -> serde_json::Value {
    let client = http_client();
    let request = |path| {
        client
            .post(format!("{base}{path}"))
            .header("session-id", &manager.0)
            .header("cookie", &manager.1)
            .header("operation-id", Uuid::new_v4().to_string())
            .header("x-real-ip", format!("203.0.116.{source}"))
    };
    let response = request("/api/v1/assignments/candidates")
        .json(&serde_json::json!({"mobile":mobile}))
        .send()
        .await
        .expect("real candidate confirmation");
    assert_eq!(response.status(), 200);
    let candidate = response
        .json::<serde_json::Value>()
        .await
        .expect("candidate");
    let response = request("/api/v1/assignments")
        .json(&serde_json::json!({
            "target": {
                "accountId":candidate["identity"]["accountId"],
                "personId":candidate["identity"]["personId"],
                "revisionId":candidate["identity"]["revisionId"],
                "mobile":mobile,
                "assignmentId":candidate["assignmentId"]
            },
            "role":{"id":role["id"],"changeId":role["changeId"]}
        }))
        .send()
        .await
        .expect("real institutional replacement");
    assert_eq!(response.status(), 201);
    response.json().await.expect("permanent assignment")
}
