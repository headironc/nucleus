//! Badge component acceptance through the real HTTP service and runtime database identity.

use std::{io::Cursor, time::Duration};

use nucleus::process::{self, Application};
use serde_json::{Value, json};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*};

#[test]
fn fixed_badges_use_trusted_preview_and_private_resources_without_request_deduplication() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(120), Box::pin(async {
            let name=database(&server).await;
            let mut supplier=Supplier::echoing().await;
            let mut sms=fixture::supplier::Supplier::start().await;
            let mut background=Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(85,54).write_to(&mut background,image::ImageFormat::Png).expect("fictional background");
            let mut template=serde_json::to_value(domain::workforce::application::fixture_template()).expect("template");
            template["fontId"]=json!(Uuid::new_v4());
            template["layout"]["department"]=json!({"area":{"xUm":4000,"yUm":30000,"widthUm":120,"heightUm":20000},"fontSizeUm":100,"lineHeightUm":140,"wrap":true});
            let pickup="P".repeat(4094);
            let settings=domain::workforce::application::Settings::new(avatar_storage(),serde_json::from_value(template).expect("template values"),"image/png",background.into_inner(),include_bytes!("../../domain/tests/fixtures/fonts/NotoSansSC-Regular.otf").to_vec(),vec![pickup.clone()]).expect("legal CJK fixture");
            let application=Application::start(connections(&server,&name,"clerk"),keys(0),services(8).expect("services").with_application(settings),process::Settings::new(Duration::from_secs(10)).expect("stop"),supplier.client(),message_at(sms.address(),sms.authority())).await.expect("application");
            let (base,stop,task)=running(application).await;
            let institution=issued_session_for(&base,&mut sms,"13900000000").await;
            let manager=issued_session_for(&base,&mut sms,"13800138000").await;
            let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(10)).build().expect("HTTP client");
            let source=std::cell::Cell::new(1_u16);
            let request=|method:reqwest::Method,path:&str,actor:&(String,String),operation:bool| {
                let address=source.get();source.set(address+1);
                let request=client.request(method,format!("{base}{path}")).header("session-id",&actor.0).header("cookie",&actor.1).header("origin","https://example.com").header("x-real-ip",format!("203.0.173.{}",address));
                if operation {request.header("operation-id",Uuid::new_v4().to_string())} else {request}
            };
            let post=|path:&str,actor:&(String,String)|request(reqwest::Method::POST,path,actor,true);
            let get=|path:&str,actor:&(String,String)|request(reqwest::Method::GET,path,actor,true);
            let role=post("/api/v1/workforce/roles",&institution).json(&json!({"entry":"institution","name":"Badge office","level":100,"permissions":["workforce.directory"]})).send().await.expect("role").json::<Value>().await.expect("role")["role"].clone();
            let candidate=post("/api/v1/workforce/grants/candidates",&institution).json(&json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate").json::<Value>().await.expect("candidate");
            let admitted=post("/api/v1/workforce/grants",&institution).json(&json!({"entry":"institution","target":{"accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],"mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]},"role":{"id":role["id"],"changeId":role["changeId"]}})).send().await.expect("admission");
            assert_eq!(admitted.status(),201);
            assert_eq!(request(reqwest::Method::GET,"/api/v1/workforce/application-template",&manager,false).send().await.expect("missing correlation").status(),400);
            let choices=get("/api/v1/workforce/application-template",&manager).send().await.expect("template before number");
            assert_eq!(choices.status(),200);
            let choices=choices.json::<Value>().await.expect("template");
            assert_eq!(choices["pickups"],json!([pickup]));
            assert_eq!(post("/api/v1/workforce/numbers",&manager).json(&json!({"subtype":100})).send().await.expect("first number").status(),201);
            let group=post("/api/v1/workforce/directories/groups",&manager).json(&json!({"name":"Group","enabled":true,"index":0})).send().await.expect("group").json::<Value>().await.expect("group")["item"].clone();
            let department=post("/api/v1/workforce/directories/departments",&manager).json(&json!({"name":"科".repeat(64),"enabled":true,"index":0,"groupId":group["id"],"parentId":null})).send().await.expect("department").json::<Value>().await.expect("department")["item"].clone();
            let mut image=Cursor::new(Vec::new());
            image::DynamicImage::new_rgba8(131,127).write_to(&mut image,image::ImageFormat::Png).expect("non matching aspect ratio with alpha");
            let photo=post("/api/v1/workforce/application-photos",&manager).header("content-type","image/png").body(image.into_inner()).send().await.expect("prepare photo");
            assert_eq!(photo.status(),201);
            let photo=photo.json::<Value>().await.expect("photo");
            assert_eq!((photo["width"].as_u64(),photo["height"].as_u64()),(Some(131),Some(127)));
            let selections=json!({"departmentId":department["id"],"rosterId":null,"titleId":null,"photoId":photo["photoId"],"pickup":pickup});
            assert_eq!(post("/api/v1/workforce/applications/preview",&manager).json(&json!({"kind":"issuance","selections":selections})).send().await.expect("incomplete rules").status(),503);
            let mut cadence=tokio::time::interval(Duration::from_millis(150));
            cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            for subtype in [100,200,300,400,500,600,700,801,802,803,804,901,902,903,904,1000] {
                cadence.tick().await;
                assert_eq!(request(reqwest::Method::PUT,&format!("/api/v1/workforce/rules/{subtype}"),&manager,true).json(&json!({"departmentId":department["id"],"position":true})).send().await.expect("rule").status(),201);
            }
            let mut preview_body=serde_json::to_vec(&json!({"kind":"issuance","selections":selections})).expect("preview input");
            preview_body.resize(8193,b' ');
            assert_eq!(post("/api/v1/workforce/applications/preview",&manager).header("content-type","application/json").body(preview_body.clone()).send().await.expect("preview budget refusal").status(),413);
            preview_body.truncate(8192);
            let preview=post("/api/v1/workforce/applications/preview",&manager).header("content-type","application/json").body(preview_body).send().await.expect("preview at its inclusive limit");
            assert_eq!(preview.status(),200);
            let preview=preview.json::<Value>().await.expect("trusted snapshot");
            assert!(serde_json::to_vec(&preview["snapshot"]).expect("full snapshot").len()>8192,"a complete legal confirmation can exceed the preview input budget");
            assert_eq!(preview["photo"],json!({"photoId":photo["photoId"],"resourceId":photo["resourceId"],"width":131,"height":127,"mediaType":"image/webp"}));
            let mut forged=preview["snapshot"].clone();forged["name"]=json!("Forged");
            let mismatch=post("/api/v1/workforce/applications",&manager).json(&json!({"kind":"issuance","selections":selections,"expected":forged})).send().await.expect("mismatch");
            assert_eq!(mismatch.status(),409);assert_eq!(mismatch.json::<Value>().await.expect("rejection"),json!({"code":"data_changed"}));
            let mut oversized=serde_json::to_vec(&json!({"kind":"issuance","selections":selections,"expected":preview["snapshot"]})).expect("confirmed content");
            oversized.resize(128*1024+1,b' ');
            assert_eq!(post("/api/v1/workforce/applications",&manager).header("content-type","application/json").body(oversized).send().await.expect("submission budget refusal").status(),413);
            let same=Uuid::new_v4().to_string();
            let mut ids=Vec::new();
            for (index,kind) in ["issuance","duplication","replacement","issuance"].into_iter().enumerate() {
                let mut confirmed=serde_json::to_vec(&json!({"kind":kind,"selections":selections,"expected":preview["snapshot"]})).expect("complete confirmation");
                if index==0 {confirmed.resize(128*1024,b' ');}
                let created=client.post(format!("{base}/api/v1/workforce/applications")).header("operation-id",&same).header("session-id",&manager.0).header("cookie",&manager.1).header("origin","https://example.com").header("x-real-ip","203.0.174.1").header("content-type","application/json").body(confirmed).send().await.expect("submit");
                assert_eq!(created.status(),201);
                assert_eq!(created.headers()["cache-control"],"no-store");assert!(!created.headers().contains_key("set-cookie"));
                let created=created.json::<Value>().await.expect("application");
                assert_eq!(created["application"]["snapshot"],preview["snapshot"]);
                ids.push(created["application"]["id"].as_str().expect("id").to_owned());
            }
            let listed=get("/api/v1/workforce/current/applications",&manager).send().await.expect("list").json::<Value>().await.expect("list");
            assert_eq!(listed["access"],"full");assert_eq!(listed["count"],4);
            let detail=get(&format!("/api/v1/workforce/current/applications/{}",ids[0]),&manager).send().await.expect("detail").json::<Value>().await.expect("detail");
            assert_eq!(detail["application"]["snapshot"],preview["snapshot"]);
            assert_eq!(get(&format!("/api/v1/workforce/current/applications/{}",ids[0]),&institution).send().await.expect("concealed").status(),404);
            let resource_path=format!("/api/v1/workforce/application-resources/{}",photo["resourceId"].as_str().expect("resource id"));
            for operation in [None,Some("invalid-correlation")] {
                let mut resource=request(reqwest::Method::GET,&resource_path,&manager,false);
                if let Some(value)=operation {resource=resource.header("operation-id",value);}
                let resource=resource.send().await.expect("resource correlation exemption");
                assert_eq!(resource.status(),200);assert_eq!(resource.headers()["content-type"],"image/webp");
                let decoded=image::load_from_memory(&resource.bytes().await.expect("private bytes")).expect("whole prepared image").to_rgba8();
                assert_eq!(decoded.dimensions(),(131,127));assert_eq!(decoded.get_pixel(0,0).0,[0,0,0,0]);
            }
            assert_eq!(request(reqwest::Method::GET,&resource_path,&institution,false).send().await.expect("resource denied").status(),404);
            assert_eq!(get("/api/v1/workforce/current/applications/not-a-uuid",&manager).send().await.expect("path").status(),400);
            let head=request(reqwest::Method::HEAD,&resource_path,&manager,false).send().await.expect("HEAD");assert_eq!(head.status(),200);assert!(head.bytes().await.expect("no HEAD body").is_empty());
            let schema=client.get(format!("{base}/openapi/v1.json")).send().await.expect("schema").json::<Value>().await.expect("OpenAPI");
            assert!(schema["paths"]["/api/v1/workforce/applications"]["post"]["parameters"].as_array().expect("headers").iter().any(|parameter|parameter["name"]=="Operation-Id"));
            assert!(!schema["paths"]["/api/v1/workforce/application-resources/{id}"]["get"]["parameters"].as_array().expect("resource headers").iter().any(|parameter|parameter["name"]=="Operation-Id"));
            stop.send(()).expect("stop HTTP");let (mut application,result)=task.await.expect("HTTP exited");result.expect("HTTP completed");application.stop().await.expect("drain");supplier.task.abort();assert!((&mut supplier.task).await.expect_err("fixture task aborted").is_cancelled());sms.stop().await;
        })).await.expect("bounded HTTP badge acceptance");
    }));
}
