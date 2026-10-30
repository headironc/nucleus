use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*};

#[test]
fn workforce_invitation_creation_returns_the_permanent_code_through_typed_http() {
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
            let request = async |method: reqwest::Method, path: &str, actor: &(String, String), operation: Uuid| {
                // Stay below the shared ten-writes-per-second HTTP budget.
                tokio::time::sleep(Duration::from_millis(110)).await;
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", operation.to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.167.{address}"))
            };
            let response = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", &manager, Uuid::new_v4()).await
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("real candidate");
            assert_eq!(response.status(), 200);
            let candidate = response.json::<serde_json::Value>().await.expect("confirmation");
            let roles = candidate["roles"].as_array().expect("choices");
            let full = roles.iter().find(|role| role["kind"] == "full").expect("full role").clone();
            let default = roles.iter().find(|role| role["kind"] == "default").expect("default role").clone();
            let admission = serde_json::json!({"entry":"institution","target":{
                "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
            },"role":{"id":full["id"],"changeId":full["changeId"]}});
            assert_eq!(request(reqwest::Method::POST, "/api/v1/workforce/grants", &manager, Uuid::new_v4()).await.json(&admission).send().await.expect("real admission").status(), 201);
            let path = "/api/v1/workforce/invitations";
            let expires_at = chrono::SubsecRound::trunc_subsecs(chrono::Utc::now() + chrono::Duration::days(3), 6);
            let body = serde_json::json!({"role":{"id":default["id"],"changeId":default["changeId"]},"capacity":5,"expiresAt":expires_at});
            let response = request(reqwest::Method::POST, path, &manager, Uuid::new_v4()).await.json(&body).send().await.expect("institution only");
            assert_eq!(response.status(), 403);
            assert_eq!(response.json::<serde_json::Value>().await.expect("denial"), serde_json::json!({"code":"permission_denied"}));
            let operation = Uuid::new_v4();
            let response = request(reqwest::Method::POST, path, &member, operation).await.json(&body).send().await.expect("creation");
            assert_eq!(response.status(), 201);
            assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let created = response.json::<serde_json::Value>().await.expect("invitation");
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
            let row = observer.client().query_one("SELECT id, role_id, capacity, expires_at, created_at FROM access.invitations WHERE operation_id = $1", &[&operation]).await.expect("one permanent invitation");
            let code = created["invitation"]["code"].as_str().expect("creator receives the original code").to_owned();
            assert_eq!(code.len(), 43);
            assert_eq!(created, serde_json::json!({"code":"ok","invitation":{"id":row.get::<_,Uuid>("id"),"roleId":row.get::<_,Uuid>("role_id"),"capacity":row.get::<_,i64>("capacity"),
                "expiresAt":row.get::<_,chrono::DateTime<chrono::Utc>>("expires_at"),"createdAt":row.get::<_,chrono::DateTime<chrono::Utc>>("created_at"),"operationId":operation,"reasons":[],"code":code}}));
            let response = request(reqwest::Method::POST, path, &member, operation).await.json(&body).send().await.expect("replay");
            assert_eq!(response.status(), 201);
            assert_eq!(response.json::<serde_json::Value>().await.expect("original invitation"), created);
            let mut changed = body.clone(); changed["capacity"] = serde_json::json!(6);
            let response = request(reqwest::Method::POST, path, &member, operation).await.json(&changed).send().await.expect("changed replay");
            assert_eq!(response.status(), 409);
            assert_eq!(response.json::<serde_json::Value>().await.expect("mismatch"), serde_json::json!({"code":"request_mismatch"}));
            let past = serde_json::json!({"role":{"id":default["id"],"changeId":default["changeId"]},"capacity":1,"expiresAt":chrono::SubsecRound::trunc_subsecs(chrono::Utc::now() - chrono::Duration::seconds(5), 6)});
            for (value, status, code) in [
                (past, 422, "expiry_passed"),
                (serde_json::json!({"role":{"id":full["id"],"changeId":full["changeId"]},"capacity":1,"expiresAt":expires_at}), 403, "permission_denied"),
                (serde_json::json!({"role":{"id":default["id"],"changeId":full["changeId"]},"capacity":1,"expiresAt":expires_at}), 409, "role_changed"),
                (serde_json::json!({"role":{"id":default["id"],"changeId":default["changeId"]},"capacity":0,"expiresAt":expires_at}), 422, "invalid_input"),
                (serde_json::json!({"role":{"id":default["id"],"changeId":default["changeId"]},"capacity":1,"expiresAt":expires_at,"code":"x"}), 422, "invalid_input"),
                (serde_json::json!({"role":{"id":default["id"]},"capacity":1,"expiresAt":expires_at}), 422, "invalid_input"),
            ] {
                let response = request(reqwest::Method::POST, path, &member, Uuid::new_v4()).await.json(&value).send().await.expect("rejected creation");
                assert_eq!(response.status(), status, "{code}");
                assert_eq!(response.json::<serde_json::Value>().await.expect("stable code"), serde_json::json!({"code":code}));
            }
            assert_eq!(request(reqwest::Method::POST, path, &member, Uuid::new_v4()).await.header("origin", "https://wrong.invalid").json(&body).send().await.expect("wrong Origin").status(), 403);
            let candidate_path = "/api/v1/workforce/invitations/candidates";
            let acceptance_path = "/api/v1/workforce/acceptances";
            let check = serde_json::json!({"code":code});
            let response = request(reqwest::Method::POST, candidate_path, &manager, Uuid::new_v4()).await.json(&check).send().await.expect("holder checks raw code");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let candidate = response.json::<serde_json::Value>().await.expect("minimal candidate");
            assert_eq!(candidate, serde_json::json!({"code":"ok","candidate":{"role":{"name":default["name"],"level":default["level"],"changeId":default["changeId"]},"creatorName":"F*","expiresAt":expires_at}}));
            assert_eq!(observer.client().query_one("SELECT count(*) FROM access.acceptances", &[]).await.expect("check consumes nothing").get::<_,i64>(0),0);
            let acceptance = serde_json::json!({"code":code,"changeId":candidate["candidate"]["role"]["changeId"]});
            for (route, value, status, error) in [
                (candidate_path.to_owned(), serde_json::json!({"code":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}), 409, "invitation_unavailable"),
                (candidate_path.to_owned(), serde_json::json!({"code":"invalid"}), 422, "invalid_input"),
                (format!("{candidate_path}?code=hidden"), check.clone(), 400, "invalid_input"),
                (candidate_path.to_owned(), serde_json::json!({"code":code,"capacity":1}), 422, "invalid_input"),
                (acceptance_path.to_owned(), serde_json::json!({"code":code}), 422, "invalid_input"),
                (acceptance_path.to_owned(), serde_json::json!({"code":code,"changeId":Uuid::new_v4()}), 409, "role_changed"),
                (format!("{acceptance_path}?code=hidden"), acceptance.clone(), 400, "invalid_input"),
            ] {
                let response=request(reqwest::Method::POST,&route,&manager,Uuid::new_v4()).await.json(&value).send().await.expect("rejected holder input");
                assert_eq!(response.status(),status,"{route}");
                assert_eq!(response.json::<serde_json::Value>().await.expect("uniform error"),serde_json::json!({"code":error}));
            }
            for (route, value) in [(candidate_path, &check),(acceptance_path,&acceptance)] {
                assert_eq!(request(reqwest::Method::POST,route,&manager,Uuid::new_v4()).await.header("origin","https://wrong.invalid").json(value).send().await.expect("holder origin rejection").status(),403);
            }
            let accepted_operation=Uuid::new_v4();
            let response=request(reqwest::Method::POST,acceptance_path,&manager,accepted_operation).await.json(&acceptance).send().await.expect("holder acceptance");
            assert_eq!(response.status(),201); assert_eq!(response.headers()["cache-control"],"no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let accepted=response.json::<serde_json::Value>().await.expect("grant and first assignment");
            assert_eq!(accepted["code"],"ok"); assert_eq!(accepted["grant"]["status"],"active");
            assert_eq!(accepted["assignment"]["role"],default);
            let fact=observer.client().query_one("SELECT admission.id,acceptance.invitation_id,member.assignment_id FROM access.grants admission JOIN access.acceptances acceptance ON acceptance.id=admission.id JOIN access.grants member ON member.id=admission.id AND member.termination IS NULL WHERE admission.operation_id=$1", &[&accepted_operation]).await.expect("independent HTTP acceptance persistence");
            assert_eq!(accepted["grant"]["id"],serde_json::json!(fact.get::<_,Uuid>("id"))); assert_eq!(fact.get::<_,Uuid>("invitation_id"),row.get::<_,Uuid>("id"));
            assert_eq!(accepted["assignment"]["id"],serde_json::json!(fact.get::<_,Uuid>("assignment_id")));
            let response=request(reqwest::Method::POST,acceptance_path,&manager,accepted_operation).await.json(&acceptance).send().await.expect("holder replay");
            assert_eq!(response.status(),201); assert_eq!(response.json::<serde_json::Value>().await.expect("original"),accepted);
            for (operation, value, error) in [(Uuid::new_v4(),acceptance.clone(),"already_authorized"),(accepted_operation,serde_json::json!({"code":code,"changeId":Uuid::new_v4()}),"request_mismatch")] {
                let response=request(reqwest::Method::POST,acceptance_path,&manager,operation).await.json(&value).send().await.expect("admission conflict");
                assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("error"),serde_json::json!({"code":error}));
            }
            let detail = format!("{path}/{}", row.get::<_, Uuid>("id"));
            let response = request(reqwest::Method::GET, &detail, &member, Uuid::new_v4()).await.send().await.expect("creator read");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("current invitation"), created, "a usable code reads back its original result");
            for (target, actor, status, code) in [
                (detail.clone(), &manager, 403, "permission_denied"),
                (format!("{path}/{}", Uuid::new_v4()), &member, 404, "target_unavailable"),
                (format!("{path}/invalid"), &member, 400, "invalid_input"),
                (format!("{detail}?code=1"), &member, 400, "invalid_input"),
            ] {
                let response = request(reqwest::Method::GET, &target, actor, Uuid::new_v4()).await.send().await.expect("rejected read");
                assert_eq!(response.status(), status, "{target}");
                assert_eq!(response.json::<serde_json::Value>().await.expect("stable code"), serde_json::json!({"code":code}));
            }
            let response = request(reqwest::Method::HEAD, &detail, &member, Uuid::new_v4()).await.send().await.expect("HEAD read");
            assert_eq!(response.status(), 200); assert!(response.bytes().await.expect("HEAD bytes").is_empty());
            assert_eq!(request(reqwest::Method::GET, &detail, &member, Uuid::new_v4()).await.header("origin", "https://wrong.invalid").send().await.expect("wrong Origin read").status(), 403);
            let adjustment_operation = Uuid::new_v4();
            let extended = expires_at + chrono::Duration::days(1);
            let adjustment_input = serde_json::json!({"expected":{"capacity":5,"expiresAt":expires_at},"capacity":6,"expiresAt":extended});
            let response = request(reqwest::Method::PUT, &detail, &member, adjustment_operation).await.json(&adjustment_input).send().await.expect("adjust invitation");
            assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let adjusted = response.json::<serde_json::Value>().await.expect("adjustment");
            let adjustment = observer.client().query_one("SELECT id, adjusted_at FROM access.adjustments WHERE operation_id=$1", &[&adjustment_operation]).await.expect("durable adjustment");
            assert_eq!(adjusted, serde_json::json!({"code":"ok","adjustment":{"id":adjustment.get::<_,Uuid>(0),"invitationId":row.get::<_,Uuid>("id"),"capacity":6,"expiresAt":extended,"adjustedAt":adjustment.get::<_,chrono::DateTime<chrono::Utc>>(1),"operationId":adjustment_operation}}));
            let response = request(reqwest::Method::PUT, &detail, &member, adjustment_operation).await.json(&adjustment_input).send().await.expect("adjustment replay");
            assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("original adjustment"), adjusted);
            for (actor, target, original, input, status, code) in [
                (&manager, detail.clone(), Uuid::new_v4(), adjustment_input.clone(), 403, "permission_denied"),
                (&member, format!("{detail}?unexpected=true"), Uuid::new_v4(), adjustment_input.clone(), 400, "invalid_input"),
                (&member, format!("{path}/not-a-uuid"), Uuid::new_v4(), adjustment_input.clone(), 400, "invalid_input"),
                (&member, detail.clone(), Uuid::new_v4(), adjustment_input.clone(), 409, "target_changed"),
                (&member, detail.clone(), adjustment_operation, serde_json::json!({"expected":{"capacity":5,"expiresAt":expires_at},"capacity":7,"expiresAt":extended}), 409, "request_mismatch"),
                (&member, detail.clone(), Uuid::new_v4(), serde_json::json!({"expected":{"capacity":6,"expiresAt":extended},"capacity":6,"expiresAt":extended}), 409, "state_conflict"),
                (&member, detail.clone(), Uuid::new_v4(), serde_json::json!({"expected":{"capacity":6,"expiresAt":extended},"capacity":7,"expiresAt":"2000-01-01T00:00:00Z"}), 422, "expiry_passed"),
                (&member, detail.clone(), Uuid::new_v4(), serde_json::json!({"expected":{"capacity":6,"expiresAt":extended},"capacity":0,"expiresAt":extended}), 422, "invalid_input"),
                (&member, detail.clone(), Uuid::new_v4(), serde_json::json!({"capacity":7,"expiresAt":extended}), 422, "invalid_input"),
                (&member, detail.clone(), Uuid::new_v4(), serde_json::json!({"expected":{"capacity":6,"expiresAt":extended,"unknown":true},"capacity":7,"expiresAt":extended}), 422, "invalid_input"),
            ] {
                let response = request(reqwest::Method::PUT, &target, actor, original).await.json(&input).send().await.expect("adjustment rejection");
                assert_eq!(response.status(), status, "{target}"); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("rejection"), serde_json::json!({"code":code}));
            }
            for target in [format!("{path}/not-a-uuid"), format!("{detail}?unexpected=true")] {
                for (content_type, body) in [("application/json", "{"), ("application/json", "{}"), ("text/plain", "{}")] {
                    let response = request(reqwest::Method::PUT, &target, &member, Uuid::new_v4()).await
                        .header("content-type", content_type).body(body).send().await.expect("path and query precede JSON rejection");
                    assert_eq!(response.status(), 400, "{target} {content_type} {body}");
                    assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(response.json::<serde_json::Value>().await.expect("metadata rejection"), serde_json::json!({"code":"invalid_input"}));
                }
            }
            let mut missing_origin = request(reqwest::Method::PUT, &detail, &member, Uuid::new_v4()).await.json(&adjustment_input).build().expect("request");
            missing_origin.headers_mut().remove("origin");
            assert_eq!(client.execute(missing_origin).await.expect("required Origin").status(), 403);
            let current = request(reqwest::Method::GET, &detail, &member, Uuid::new_v4()).await.send().await.expect("adjusted detail").json::<serde_json::Value>().await.expect("detail");
            assert_eq!(current["invitation"]["capacity"], 6); assert_eq!(current["invitation"]["expiresAt"], serde_json::json!(extended)); assert_eq!(current["invitation"]["code"], code);
            let withdrawals = format!("{detail}/withdrawals");
            for (target, actor, body, status, code) in [
                (withdrawals.clone(), &manager, "", 403, "permission_denied"),
                (format!("{path}/{}/withdrawals", Uuid::new_v4()), &member, "", 404, "target_unavailable"),
                (format!("{withdrawals}?reason=x"), &member, "", 400, "invalid_input"),
                (withdrawals.clone(), &member, "{}", 400, "invalid_input"),
            ] {
                let response = request(reqwest::Method::POST, &target, actor, Uuid::new_v4()).await.body(body).send().await.expect("rejected withdrawal");
                assert_eq!(response.status(), status, "{target} {body}");
                assert_eq!(response.json::<serde_json::Value>().await.expect("stable code"), serde_json::json!({"code":code}));
            }
            let withdrawal_operation = Uuid::new_v4();
            let response = request(reqwest::Method::POST, &withdrawals, &member, withdrawal_operation).await.send().await.expect("withdrawal");
            assert_eq!(response.status(), 201);
            assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let withdrawn = response.json::<serde_json::Value>().await.expect("withdrawal fact");
            let fact = observer.client().query_one("SELECT withdrawn_at FROM access.withdrawals WHERE operation_id = $1", &[&withdrawal_operation]).await.expect("one permanent withdrawal");
            assert_eq!(withdrawn, serde_json::json!({"code":"ok","withdrawal":{"id":row.get::<_,Uuid>("id"),"withdrawnAt":fact.get::<_,chrono::DateTime<chrono::Utc>>("withdrawn_at"),"operationId":withdrawal_operation}}));
            let response = request(reqwest::Method::POST, &withdrawals, &member, withdrawal_operation).await.send().await.expect("withdrawal replay");
            assert_eq!(response.status(), 201); assert_eq!(response.json::<serde_json::Value>().await.expect("original withdrawal"), withdrawn);
            let response = request(reqwest::Method::POST, &withdrawals, &member, Uuid::new_v4()).await.send().await.expect("second withdrawal");
            assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("conflict"), serde_json::json!({"code":"state_conflict"}));
            let current = request(reqwest::Method::GET, &detail, &member, Uuid::new_v4()).await.send().await.expect("withdrawn read").json::<serde_json::Value>().await.expect("withdrawn invitation");
            assert_eq!(current["invitation"]["reasons"], serde_json::json!(["withdrawn"]));
            assert!(current["invitation"].get("code").is_none(), "a withdrawn code is omitted");
            let response = request(reqwest::Method::PUT, &detail, &member, adjustment_operation).await.json(&adjustment_input).send().await.expect("adjustment replays after withdrawal");
            assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("original adjustment"), adjusted);
            let response = request(reqwest::Method::PUT, &detail, &member, Uuid::new_v4()).await.json(&adjustment_input).send().await.expect("cannot revive withdrawn invitation");
            assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("conflict"), serde_json::json!({"code":"state_conflict"}));
            let response=request(reqwest::Method::POST,candidate_path,&manager,Uuid::new_v4()).await.json(&check).send().await.expect("withdrawn candidate");
            assert_eq!(response.status(),409); assert_eq!(response.json::<serde_json::Value>().await.expect("uniform unavailability"),serde_json::json!({"code":"invitation_unavailable"}));
            assert_eq!(request(reqwest::Method::POST,acceptance_path,&manager,accepted_operation).await.json(&acceptance).send().await.expect("withdrawn replay").json::<serde_json::Value>().await.expect("original grant"),accepted);
            assert_eq!(observer.client().query_one("SELECT count(*) FROM access.invitations", &[]).await.expect("rejections write nothing").get::<_, i64>(0), 1);
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let route = &document["paths"][path]["post"];
            assert_eq!(document["paths"][candidate_path]["post"]["operationId"],"checkWorkforceInvitation");
            assert_eq!(document["paths"][acceptance_path]["post"]["operationId"],"acceptWorkforceInvitation");
            assert!(document["paths"][candidate_path]["post"]["responses"].get("409").is_some());
            assert!(document["paths"][acceptance_path]["post"]["responses"].get("201").is_some());
            assert_eq!(route["operationId"], "createWorkforceInvitation");
            assert_eq!(route["requestBody"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/invitation.Input");
            assert_eq!(route["responses"]["201"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/grant.invitation.Receipt");
            assert_eq!(document["components"]["schemas"]["invitation.Capacity"]["minimum"], 1);
            let read = &document["paths"]["/api/v1/workforce/invitations/{id}"];
            assert_eq!(read["put"]["operationId"], "adjustWorkforceInvitation");
            assert_eq!(read["put"]["requestBody"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/invitation.adjustment.Input");
            assert_eq!(read["put"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/grant.invitation.adjustment.Receipt");
            assert_eq!(read["get"]["operationId"], "readWorkforceInvitation");
            assert_eq!(read["head"]["operationId"], "readWorkforceInvitationHead");
            assert_eq!(read["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/grant.invitation.Receipt");
            let withdrawal = &document["paths"]["/api/v1/workforce/invitations/{id}/withdrawals"]["post"];
            assert_eq!(withdrawal["operationId"], "withdrawWorkforceInvitation");
            assert_eq!(withdrawal["responses"]["201"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/grant.invitation.withdrawal.Receipt");
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop"); let (application, ended) = task.await.expect("process"); ended.expect("drained"); drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
            assert!(timeout(Duration::from_millis(30), sms.request()).await.is_err()); sms.stop().await;
        }).await.expect("bounded invitation HTTP acceptance");
    });
}
