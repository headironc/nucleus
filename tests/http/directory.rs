use std::time::Duration;

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*};

#[test]
fn workforce_directories_manage_all_collections_through_http() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(60), Box::pin(async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let institution = issued_session_for(&base, &mut sms, "13900000000").await;
            let manager = issued_session_for(&base, &mut sms, "13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = std::cell::Cell::new(1_u8);
            let request = |method: reqwest::Method, path: &str, actor: &(String, String), operation: Uuid| {
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", operation.to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.172.{address}"))
            };
            let role = request(reqwest::Method::POST, "/api/v1/workforce/roles", &institution, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","name":"Directory manager","level":100,"permissions":["workforce.directory"]})).send().await.expect("directory-only role");
            assert_eq!(role.status(), 201);
            let role = role.json::<serde_json::Value>().await.expect("role")["role"].clone();
            let candidate = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", &institution, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate").json::<serde_json::Value>().await.expect("confirmation");
            let admission = serde_json::json!({"entry":"institution","target":{
                "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
            },"role":{"id":role["id"],"changeId":role["changeId"]}});
            assert_eq!(request(reqwest::Method::POST, "/api/v1/workforce/grants", &institution, Uuid::new_v4()).json(&admission).send().await.expect("admit manager").status(), 201);
            let groups = "/api/v1/workforce/directories/groups";
            let departments = "/api/v1/workforce/directories/departments";
            let positions = "/api/v1/workforce/directories/positions";
            assert_eq!(request(reqwest::Method::GET, groups, &institution, Uuid::new_v4()).send().await.expect("institution is not directory permission").status(), 403);
            let operation = Uuid::new_v4();
            let body = serde_json::json!({"name":"  Cafe\u{301}  ","enabled":false,"index":0});
            let response = request(reqwest::Method::POST, groups, &manager, operation).json(&body).send().await.expect("create group");
            assert_eq!(response.status(), 201);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie") && !response.headers().contains_key("operation-id"));
            let group = response.json::<serde_json::Value>().await.expect("group")["item"].clone();
            assert_eq!(group["name"], "Café"); assert_eq!(group["operationId"], operation.to_string());
            let group_path = format!("{groups}/{}", group["id"].as_str().expect("id"));
            let department_body = serde_json::json!({"name":"Clinical","enabled":true,"index":2,"groupId":group["id"],"parentId":null});
            let department_operation = Uuid::new_v4();
            let response = request(reqwest::Method::POST, departments, &manager, department_operation).json(&department_body).send().await.expect("create department");
            assert_eq!(response.status(), 201);
            let department = response.json::<serde_json::Value>().await.expect("department")["item"].clone();
            let department_path = format!("{departments}/{}", department["id"].as_str().expect("id"));
            let response = request(reqwest::Method::POST, groups, &manager, Uuid::new_v4()).json(&serde_json::json!({"name":"Clinical","enabled":true,"index":0})).send().await.expect("cross-table name");
            assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("code")["code"], "name_in_use");
            let position_body = serde_json::json!({"name":"Clinical","enabled":true,"index":1});
            let group_input = group_creation(&position_body);
            let position_operation = Uuid::new_v4();
            let response = request(reqwest::Method::POST, positions, &manager, position_operation).json(&group_input).send().await.expect("group-shaped body creates a position on the positions path");
            assert_eq!(response.status(), 201);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie") && !response.headers().contains_key("operation-id"));
            let position = response.json::<serde_json::Value>().await.expect("position")["item"].clone();
            assert_eq!(position["name"], "Clinical"); assert_eq!(position["operationId"], position_operation.to_string());
            assert_eq!(typed_position_id(&position), position["id"]);
            assert_ne!(position["id"], group["id"]);
            let position_path = format!("{positions}/{}", position["id"].as_str().expect("id"));
            // Pace additional probes below the shared ten-write-per-second ingress budget.
            for (collection, item) in [(positions, &group), (groups, &position)] {
                let path = format!("{collection}/{}", item["id"].as_str().expect("foreign collection ID"));
                for (method, body) in [
                    (reqwest::Method::PUT, serde_json::json!({"expected":item,"replacement":item})),
                    (reqwest::Method::DELETE, serde_json::json!({"expected":item})),
                ] {
                    tokio::time::sleep(Duration::from_millis(125)).await;
                    let response = request(method, &path, &manager, Uuid::new_v4()).json(&body).send().await.expect("foreign collection ID");
                    assert_eq!(response.status(), 404);
                    assert_eq!(response.json::<serde_json::Value>().await.expect("absent target"), serde_json::json!({"code":"target_unavailable"}));
                }
            }
            for (path, input) in [(positions, &department_body), (groups, &department_body), (departments, &position_body)] {
                tokio::time::sleep(Duration::from_millis(125)).await;
                assert_eq!(request(reqwest::Method::POST, path, &manager, Uuid::new_v4()).json(input).send().await.expect("collection-specific creation shape").status(), 422);
            }
            let response = request(reqwest::Method::POST, positions, &manager, Uuid::new_v4()).json(&serde_json::json!({"name":"Clinical","enabled":false,"index":0})).send().await.expect("duplicate position name");
            assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("code")["code"], "name_in_use");
            let response = request(reqwest::Method::DELETE, &group_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"expected":group})).send().await.expect("referenced group");
            assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("code")["code"], "item_in_use");
            for (path, item) in [(groups, &group), (departments, &department), (positions, &position)] {
                let response = request(reqwest::Method::GET, path, &manager, Uuid::new_v4()).send().await.expect("management read");
                assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<serde_json::Value>().await.expect("items")["items"], serde_json::json!([item]));
                let response = request(reqwest::Method::HEAD, path, &manager, Uuid::new_v4()).send().await.expect("HEAD");
                assert_eq!(response.status(), 200); assert!(response.bytes().await.expect("empty body").is_empty());
            }
            let mut replacement = department.clone(); replacement["name"] = serde_json::json!("Updated"); replacement["enabled"] = serde_json::json!(false);
            let update_operation = Uuid::new_v4();
            let update = serde_json::json!({"expected":department,"replacement":replacement});
            let response = request(reqwest::Method::PUT, &department_path, &manager, update_operation).json(&update).send().await.expect("update");
            assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("replacement")["item"], replacement);
            let mut stale = update.clone(); stale["replacement"]["name"] = serde_json::json!("Stale");
            for (operation, code) in [(Uuid::new_v4(), "data_changed"), (update_operation, "request_mismatch")] {
                let response = request(reqwest::Method::PUT, &department_path, &manager, operation).json(&stale).send().await.expect("stale or mismatch");
                assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("code")["code"], code);
            }
            let mut position_replacement = position.clone(); position_replacement["name"] = serde_json::json!("Chief"); position_replacement["enabled"] = serde_json::json!(false);
            let position_update_op = Uuid::new_v4();
            let position_update = serde_json::json!({"expected":position,"replacement":position_replacement});
            let response = request(reqwest::Method::PUT, &position_path, &manager, position_update_op).json(&position_update).send().await.expect("position update");
            assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("replacement")["item"], position_replacement);
            let mut position_stale = position_update.clone(); position_stale["replacement"]["name"] = serde_json::json!("Stale");
            for (operation, code) in [(Uuid::new_v4(), "data_changed"), (position_update_op, "request_mismatch")] {
                let response = request(reqwest::Method::PUT, &position_path, &manager, operation).json(&position_stale).send().await.expect("stale or mismatch");
                assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("code")["code"], code);
            }
            for invalid in [serde_json::json!({"name":"A"}), serde_json::json!({"name":"","enabled":true,"index":0}), serde_json::json!({"name":"A\n","enabled":true,"index":0}), serde_json::json!({"name":"A","enabled":true,"index":-1}), serde_json::json!({"name":"A","enabled":true,"index":0,"extra":0})] {
                assert_eq!(request(reqwest::Method::POST, groups, &manager, Uuid::new_v4()).json(&invalid).send().await.expect("invalid input").status(), 422);
            }
            for (method, path) in [(reqwest::Method::POST, positions), (reqwest::Method::PUT, position_path.as_str()), (reqwest::Method::DELETE, position_path.as_str())] {
                for (content_type, body, status) in [
                    ("application/json", "{".to_owned(), 400),
                    ("text/plain", "{}".to_owned(), 415),
                    ("application/json", "{}".to_owned(), 422),
                    ("application/json", " ".repeat(8193), 413),
                ] {
                    tokio::time::sleep(Duration::from_millis(125)).await;
                    let response = request(method.clone(), path, &manager, Uuid::new_v4()).header("content-type", content_type).body(body).send().await.expect("common JSON rejection");
                    assert_eq!(response.status(), status);
                    assert_eq!(response.json::<serde_json::Value>().await.expect("common error"), serde_json::json!({"code":"invalid_input"}));
                }
                tokio::time::sleep(Duration::from_millis(125)).await;
                let response = request(method, &format!("{path}?extra=1"), &manager, Uuid::new_v4()).header("content-type", "application/json").body(" ".repeat(8193)).send().await.expect("query before body");
                assert_eq!(response.status(), 400);
            }
            for path in ["/api/v1/workforce/directories/unknown", "/api/v1/workforce/directories/groups/not-a-uuid"] {
                let method = if path.ends_with("not-a-uuid") { reqwest::Method::PUT } else { reqwest::Method::GET };
                assert_eq!(request(method, path, &manager, Uuid::new_v4()).header("content-type", "application/json").body(" ".repeat(8193)).send().await.expect("closed path before body").status(), 400);
            }
            assert_eq!(request(reqwest::Method::GET, &format!("{groups}?page=1"), &manager, Uuid::new_v4()).send().await.expect("query rejected").status(), 400);
            assert_eq!(request(reqwest::Method::GET, groups, &manager, Uuid::new_v4()).body("{}").send().await.expect("body rejected").status(), 400);
            assert_eq!(request(reqwest::Method::POST, groups, &manager, Uuid::new_v4()).header("origin", "https://wrong.invalid").json(&body).send().await.expect("Origin").status(), 403);
            tokio::time::sleep(Duration::from_millis(600)).await;
            for (path, expected) in [(&position_path, &position_replacement), (&department_path, &replacement), (&group_path, &group)] {
                let response = request(reqwest::Method::DELETE, path, &manager, Uuid::new_v4()).json(&serde_json::json!({"expected":expected})).send().await.expect("delete");
                assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("deleted"), serde_json::json!({"code":"ok","deleted":true}));
            }
            let response = request(reqwest::Method::DELETE, &position_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"expected":position_replacement})).send().await.expect("new deletion of absent position");
            assert_eq!(response.status(), 404); assert_eq!(response.json::<serde_json::Value>().await.expect("absent"), serde_json::json!({"code":"target_unavailable"}));
            let response = request(reqwest::Method::DELETE, &group_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"expected":group})).send().await.expect("new deletion of an absent item");
            assert_eq!(response.status(), 404); assert_eq!(response.json::<serde_json::Value>().await.expect("absent"), serde_json::json!({"code":"target_unavailable"}));
            for (path, original, input) in [(groups, operation, body), (departments, department_operation, department_body), (positions, position_operation, position_body)] {
                let response = request(reqwest::Method::POST, path, &manager, original).json(&input).send().await.expect("deleted creation replay");
                assert_eq!(response.status(), 201); assert_eq!(response.json::<serde_json::Value>().await.expect("deleted replay"), serde_json::json!({"code":"ok","deleted":true}));
            }
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            for (path, methods) in [("/api/v1/workforce/directories/{collection}", &["get", "head", "post"][..]), ("/api/v1/workforce/directories/{collection}/{id}", &["put", "delete"][..])] {
                for method in methods { assert!(document["paths"][path][method].is_object()); }
            }
            assert!(document["paths"]["/api/v1/workforce/directories/{collection}/{id}"]["delete"]["responses"]["404"].is_object(), "absent deletion is documented");
            stop.send(()).expect("stop"); let (application, result) = task.await.expect("server task"); result.expect("shutdown"); drop(application); supplier.task.abort(); sms.stop().await;
        })).await.expect("bounded directory HTTP acceptance");
    }));
}

#[test]
fn workforce_rules_configure_through_real_http() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(90), Box::pin(async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let institution = issued_session_for(&base, &mut sms, "13900000000").await;
            let manager = issued_session_for(&base, &mut sms, "13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = std::cell::Cell::new(1_u8);
            let request = |method: reqwest::Method, path: &str, actor: &(String, String), operation: Uuid| {
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", operation.to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.173.{address}"))
            };
            let role = request(reqwest::Method::POST, "/api/v1/workforce/roles", &institution, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","name":"Rule manager","level":100,"permissions":["workforce.directory"]})).send().await.expect("directory-only role");
            assert_eq!(role.status(), 201);
            let role = role.json::<serde_json::Value>().await.expect("role")["role"].clone();
            let candidate = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", &institution, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate").json::<serde_json::Value>().await.expect("confirmation");
            let admission = serde_json::json!({"entry":"institution","target":{
                "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
            },"role":{"id":role["id"],"changeId":role["changeId"]}});
            assert_eq!(request(reqwest::Method::POST, "/api/v1/workforce/grants", &institution, Uuid::new_v4()).json(&admission).send().await.expect("admit manager").status(), 201);
            let rules = "/api/v1/workforce/rules";
            assert_eq!(request(reqwest::Method::GET, rules, &institution, Uuid::new_v4()).send().await.expect("institution permission").status(), 403);
            let response = request(reqwest::Method::GET, rules, &manager, Uuid::new_v4()).send().await.expect("initial rules");
            assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.json::<serde_json::Value>().await.expect("missing rules"), serde_json::json!({
                "code":"ok","rules":[],"missing":[100,200,300,400,500,600,700,801,802,803,804,901,902,903,904,1000]
            }));
            let response = request(reqwest::Method::HEAD, rules, &manager, Uuid::new_v4()).send().await.expect("rules HEAD");
            assert_eq!(response.status(), 200); assert!(response.bytes().await.expect("empty HEAD").is_empty());

            let group = request(reqwest::Method::POST, "/api/v1/workforce/directories/groups", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"name":"HTTP rule group","enabled":true,"index":0})).send().await.expect("real group").json::<serde_json::Value>().await.expect("group")["item"].clone();
            let department = request(reqwest::Method::POST, "/api/v1/workforce/directories/departments", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"name":"HTTP rule department","enabled":true,"index":0,"groupId":group["id"],"parentId":null})).send().await.expect("real department").json::<serde_json::Value>().await.expect("department")["item"].clone();
            let rule_path = format!("{rules}/902");
            let create_body = serde_json::json!({"departmentId":department["id"],"position":false});
            let create_operation = Uuid::new_v4();
            let response = request(reqwest::Method::PUT, &rule_path, &manager, create_operation).json(&create_body).send().await.expect("create rule");
            assert_eq!(response.status(), 201); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let created = response.json::<serde_json::Value>().await.expect("created rule");
            assert_eq!(created, serde_json::json!({"code":"ok","rule":{"id":902,"departmentId":department["id"],"position":false}}));
            let response = request(reqwest::Method::PUT, &rule_path, &manager, create_operation).json(&create_body).send().await.expect("create replay");
            assert_eq!(response.status(), 201); assert_eq!(response.json::<serde_json::Value>().await.expect("original create"), created);
            let mut mismatched = create_body.clone(); mismatched["position"] = serde_json::json!(true);
            for (operation, body, status, code) in [
                (create_operation, mismatched, 409, "request_mismatch"),
                (Uuid::new_v4(), create_body.clone(), 409, "data_changed"),
            ] {
                let response = request(reqwest::Method::PUT, &rule_path, &manager, operation).json(&body).send().await.expect("create conflict");
                assert_eq!(response.status(), status); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":code}));
            }
            let mut updated_rule = created["rule"].clone(); updated_rule["position"] = serde_json::json!(true);
            let update_body = serde_json::json!({"expected":created["rule"],"replacement":updated_rule});
            let update_operation = Uuid::new_v4();
            let response = request(reqwest::Method::PUT, &rule_path, &manager, update_operation).json(&update_body).send().await.expect("update rule");
            assert_eq!(response.status(), 200);
            let updated = response.json::<serde_json::Value>().await.expect("updated rule");
            assert_eq!(updated["rule"], updated_rule);
            let response = request(reqwest::Method::PUT, &rule_path, &manager, update_operation).json(&update_body).send().await.expect("update replay");
            assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("original update"), updated);
            let mut stale = update_body.clone(); stale["replacement"]["position"] = serde_json::json!(false);
            for (operation, body, code) in [(Uuid::new_v4(), stale.clone(), "data_changed"), (update_operation, stale, "request_mismatch")] {
                let response = request(reqwest::Method::PUT, &rule_path, &manager, operation).json(&body).send().await.expect("stale or mismatched update");
                assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":code}));
            }
            let response = request(reqwest::Method::GET, rules, &manager, Uuid::new_v4()).send().await.expect("configured read");
            let configuration = response.json::<serde_json::Value>().await.expect("configuration");
            assert_eq!(configuration["rules"], serde_json::json!([updated_rule]));
            assert_eq!(configuration["missing"].as_array().expect("missing codes").len(), 15);
            assert!(!configuration["missing"].as_array().expect("missing codes").contains(&serde_json::json!(902)));
            let missing_rule = serde_json::json!({"id":100,"departmentId":department["id"],"position":true});
            let response = request(reqwest::Method::PUT, &format!("{rules}/100"), &manager, Uuid::new_v4())
                .json(&serde_json::json!({"expected":missing_rule,"replacement":missing_rule})).send().await.expect("update missing rule");
            assert_eq!(response.status(), 404); assert_eq!(response.json::<serde_json::Value>().await.expect("missing"), serde_json::json!({"code":"target_unavailable"}));

            let unauthenticated = (Uuid::new_v4().to_string(), String::new());
            for path in [format!("{rules}/999"), format!("{rules}/not-a-code")] {
                let response = request(reqwest::Method::PUT, &path, &unauthenticated, Uuid::new_v4()).json(&create_body).send().await.expect("closed subtype path");
                assert_eq!(response.status(), 400); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":"invalid_input"}));
            }
            assert_eq!(request(reqwest::Method::PUT, &rule_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"position":true})).send().await.expect("required department").status(), 422);
            assert_eq!(request(reqwest::Method::GET, &format!("{rules}?page=1"), &manager, Uuid::new_v4()).send().await.expect("query").status(), 400);
            assert_eq!(request(reqwest::Method::GET, rules, &manager, Uuid::new_v4()).body("{}").send().await.expect("GET body").status(), 400);
            assert_eq!(request(reqwest::Method::PUT, &format!("{rule_path}?extra=1"), &manager, Uuid::new_v4()).json(&create_body).send().await.expect("PUT query").status(), 400);
            assert_eq!(request(reqwest::Method::PUT, &rule_path, &manager, Uuid::new_v4()).header("origin", "https://wrong.invalid").json(&create_body).send().await.expect("Origin").status(), 403);

            let disabled = request(reqwest::Method::POST, "/api/v1/workforce/directories/departments", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"name":"HTTP disabled default","enabled":false,"index":0,"groupId":group["id"],"parentId":null})).send().await.expect("disabled department").json::<serde_json::Value>().await.expect("disabled")["item"].clone();
            let response = request(reqwest::Method::PUT, &format!("{rules}/100"), &manager, Uuid::new_v4())
                .json(&serde_json::json!({"departmentId":disabled["id"],"position":true})).send().await.expect("disabled default");
            assert_eq!(response.status(), 422); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":"invalid_department"}));
            let department_path = format!("/api/v1/workforce/directories/departments/{}", department["id"].as_str().expect("department id"));
            let response = request(reqwest::Method::DELETE, &department_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"expected":department})).send().await.expect("referenced deletion");
            assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":"item_in_use"}));
            let mut disabled_replacement = department.clone(); disabled_replacement["enabled"] = serde_json::json!(false);
            let response = request(reqwest::Method::PUT, &department_path, &manager, Uuid::new_v4())
                .json(&serde_json::json!({"expected":department,"replacement":disabled_replacement})).send().await.expect("later disabling");
            assert_eq!(response.status(), 200);
            let response = request(reqwest::Method::GET, rules, &manager, Uuid::new_v4()).send().await.expect("reference retained");
            assert_eq!(response.json::<serde_json::Value>().await.expect("retained")["rules"], serde_json::json!([updated_rule]));

            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let list = &document["paths"][rules];
            assert_eq!(list["get"]["operationId"], "listWorkforceRules");
            assert!(list["head"].is_object());
            let put = &document["paths"]["/api/v1/workforce/rules/{subtype}"]["put"];
            assert_eq!(put["operationId"], "configureWorkforceRule");
            assert_eq!(put["requestBody"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/directory.rule.Input");
            for status in ["200", "201", "400", "401", "403", "404", "409", "413", "415", "422", "429", "503"] {
                assert!(put["responses"].get(status).is_some(), "{status}");
            }
            stop.send(()).expect("stop"); let (application, result) = task.await.expect("server task"); result.expect("shutdown"); drop(application); supplier.task.abort(); sms.stop().await;
        })).await.expect("bounded rule HTTP acceptance");
    }));
}

#[test]
fn workforce_directory_selection_reads_through_real_http() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(90), Box::pin(async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let institution = issued_session_for(&base, &mut sms, "13900000000").await;
            let manager = issued_session_for(&base, &mut sms, "13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = std::cell::Cell::new(1_u8);
            let request = |method: reqwest::Method, path: &str, actor: &(String, String), operation: Uuid| {
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", operation.to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.170.{address}"))
            };
            let role = request(reqwest::Method::POST, "/api/v1/workforce/roles", &institution, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","name":"Selection manager","level":100,"permissions":["workforce.directory"]})).send().await.expect("directory-only role");
            assert_eq!(role.status(), 201);
            let role = role.json::<serde_json::Value>().await.expect("role")["role"].clone();
            let candidate = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", &institution, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate").json::<serde_json::Value>().await.expect("confirmation");
            let admission = serde_json::json!({"entry":"institution","target":{
                "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
            },"role":{"id":role["id"],"changeId":role["changeId"]}});
            assert_eq!(request(reqwest::Method::POST, "/api/v1/workforce/grants", &institution, Uuid::new_v4()).json(&admission).send().await.expect("admit manager").status(), 201);

            // The institution administrator holds no Workforce Grant.
            let outsider = institution.clone();
            tokio::time::sleep(Duration::from_millis(125)).await;
            let group = request(reqwest::Method::POST, "/api/v1/workforce/directories/groups", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"name":"HTTP selectable group","enabled":true,"index":0})).send().await.expect("group").json::<serde_json::Value>().await.expect("group")["item"].clone();
            let path = "/api/v1/workforce/directories";
            let response = request(reqwest::Method::GET, path, &manager, Uuid::new_v4()).send().await.expect("selection");
            assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let selection = response.json::<serde_json::Value>().await.expect("selection JSON");
            let keys: Vec<&str> = selection.as_object().expect("object").keys().map(String::as_str).collect();
            assert_eq!(keys, vec!["categories", "code", "departments", "disciplines", "groups", "missing", "positions", "rules", "titles"]);
            assert_eq!(selection["code"], "ok");
            assert_eq!(selection["groups"], serde_json::json!([{"id":group["id"],"name":"HTTP selectable group","index":0}]));
            assert_eq!(selection["missing"].as_array().expect("missing").len(), 16);
            let response = request(reqwest::Method::HEAD, path, &manager, Uuid::new_v4()).send().await.expect("HEAD");
            assert_eq!(response.status(), 200); assert!(response.bytes().await.expect("empty HEAD").is_empty());
            let unauthenticated = (Uuid::new_v4().to_string(), String::new());
            for (actor, query, body, status, code) in [
                (&outsider, "", "", 403, "permission_denied"),
                (&unauthenticated, "", "", 401, "authentication_invalid"),
                (&manager, "?page=1", "", 400, "invalid_input"),
                (&manager, "", "{}", 400, "invalid_input"),
            ] {
                let response = request(reqwest::Method::GET, &format!("{path}{query}"), actor, Uuid::new_v4()).body(body).send().await.expect("rejection");
                assert_eq!(response.status(), status, "{code}"); assert_eq!(response.headers()["cache-control"], "no-store");
                assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":code}));
            }
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let operation = &document["paths"][path]["get"];
            assert_eq!(operation["operationId"], "readWorkforceDirectorySelection");
            assert!(document["paths"][path]["head"].is_object());
            for status in ["200", "400", "401", "403", "429", "503"] {
                assert!(operation["responses"].get(status).is_some(), "{status}");
            }
            for schema in ["Group", "Department", "Position", "Category", "Discipline", "Title", "Rule"] {
                assert!(document["components"]["schemas"][format!("directory.selection.{schema}")]["properties"].is_object(), "{schema}");
            }
            stop.send(()).expect("stop"); let (application, result) = task.await.expect("server task"); result.expect("shutdown"); drop(application); supplier.task.abort(); sms.stop().await;
        })).await.expect("bounded directory selection HTTP acceptance");
    }));
}

#[test]
fn workforce_title_tree_manages_through_real_http() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(90), Box::pin(async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let institution = issued_session_for(&base, &mut sms, "13900000000").await;
            let manager = issued_session_for(&base, &mut sms, "13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = std::cell::Cell::new(1_u8);
            let request = |method: reqwest::Method, path: &str, actor: &(String, String), operation: Uuid| {
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", operation.to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.171.{address}"))
            };
            let role = request(reqwest::Method::POST, "/api/v1/workforce/roles", &institution, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","name":"Title manager","level":100,"permissions":["workforce.directory"]})).send().await.expect("directory-only role");
            assert_eq!(role.status(), 201);
            let role = role.json::<serde_json::Value>().await.expect("role")["role"].clone();
            let candidate = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", &institution, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate").json::<serde_json::Value>().await.expect("confirmation");
            let admission = serde_json::json!({"entry":"institution","target":{
                "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
            },"role":{"id":role["id"],"changeId":role["changeId"]}});
            assert_eq!(request(reqwest::Method::POST, "/api/v1/workforce/grants", &institution, Uuid::new_v4()).json(&admission).send().await.expect("admit manager").status(), 201);

            let created = |path: &'static str, body: serde_json::Value| {
                let request = &request; let manager = &manager;
                async move {
                    tokio::time::sleep(Duration::from_millis(125)).await;
                    let response = request(reqwest::Method::POST, &format!("/api/v1/workforce/directories/{path}"), manager, Uuid::new_v4()).json(&body).send().await.expect("create");
                    assert_eq!(response.status(), 201, "{path}");
                    assert_eq!(response.headers()["cache-control"], "no-store");
                    response.json::<serde_json::Value>().await.expect("item")["item"].clone()
                }
            };
            let category = created("categories", serde_json::json!({"name":"HTTP medicine","enabled":true,"index":0})).await;
            let discipline = created("disciplines", serde_json::json!({"name":"HTTP surgery","enabled":true,"index":0,"categoryId":category["id"]})).await;
            let title = created("titles", serde_json::json!({"name":"HTTP chief surgeon","enabled":false,"index":3,"disciplineId":discipline["id"]})).await;
            assert_eq!(title["disciplineId"], discipline["id"]); assert_eq!(title.as_object().expect("title").len(), 6);
            for (path, item) in [("categories", &category), ("disciplines", &discipline), ("titles", &title)] {
                let response = request(reqwest::Method::GET, &format!("/api/v1/workforce/directories/{path}"), &manager, Uuid::new_v4()).send().await.expect("management read");
                assert_eq!(response.status(), 200);
                assert_eq!(response.json::<serde_json::Value>().await.expect("items")["items"], serde_json::json!([item]));
            }
            assert_eq!(request(reqwest::Method::GET, "/api/v1/workforce/directories/titles", &institution, Uuid::new_v4()).send().await.expect("institution permission").status(), 403);
            for (path, body, status, code) in [
                ("disciplines", serde_json::json!({"name":"HTTP medicine","enabled":true,"index":0,"categoryId":category["id"]}), 409, "name_in_use"),
                ("categories", serde_json::json!({"name":"HTTP chief surgeon","enabled":true,"index":0}), 409, "name_in_use"),
                ("titles", serde_json::json!({"name":"HTTP orphan","enabled":true,"index":0,"disciplineId":category["id"]}), 404, "target_unavailable"),
                ("disciplines", serde_json::json!({"name":"HTTP orphan","enabled":true,"index":0}), 422, "invalid_input"),
                ("categories", serde_json::json!({"name":"HTTP orphan","enabled":true,"index":0,"disciplineId":discipline["id"]}), 422, "invalid_input"),
            ] {
                tokio::time::sleep(Duration::from_millis(125)).await;
                let response = request(reqwest::Method::POST, &format!("/api/v1/workforce/directories/{path}"), &manager, Uuid::new_v4()).json(&body).send().await.expect("rejection");
                assert_eq!(response.status(), status, "{path} {code}"); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":code}));
            }
            let category_path = format!("/api/v1/workforce/directories/categories/{}", category["id"].as_str().expect("id"));
            let discipline_path = format!("/api/v1/workforce/directories/disciplines/{}", discipline["id"].as_str().expect("id"));
            let title_path = format!("/api/v1/workforce/directories/titles/{}", title["id"].as_str().expect("id"));
            for (path, item) in [(&category_path, &category), (&discipline_path, &discipline)] {
                tokio::time::sleep(Duration::from_millis(125)).await;
                let response = request(reqwest::Method::DELETE, path, &manager, Uuid::new_v4()).json(&serde_json::json!({"expected":item})).send().await.expect("referenced deletion");
                assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":"item_in_use"}));
            }
            let mut renamed = title.clone(); renamed["name"] = serde_json::json!("HTTP attending surgeon"); renamed["enabled"] = serde_json::json!(true);
            tokio::time::sleep(Duration::from_millis(125)).await;
            let response = request(reqwest::Method::PUT, &title_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"expected":title,"replacement":renamed})).send().await.expect("title update");
            assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("item")["item"], renamed);
            for (path, item) in [(&title_path, &renamed), (&discipline_path, &discipline), (&category_path, &category)] {
                tokio::time::sleep(Duration::from_millis(125)).await;
                let response = request(reqwest::Method::DELETE, path, &manager, Uuid::new_v4()).json(&serde_json::json!({"expected":item})).send().await.expect("leaf-first deletion");
                assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("deleted"), serde_json::json!({"code":"ok","deleted":true}));
            }
            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let parameter = document["paths"]["/api/v1/workforce/directories/{collection}"]["get"]["parameters"].as_array().expect("parameters").iter().find(|parameter| parameter["name"] == "collection").expect("collection parameter")["schema"].clone();
            let schema = match parameter["$ref"].as_str() {
                Some(reference) => document.pointer(reference.trim_start_matches('#')).unwrap_or_else(|| panic!("unresolved {reference}")).clone(),
                None => parameter,
            };
            let collections = schema["enum"].as_array().expect("collections").clone();
            for collection in ["groups", "departments", "positions", "categories", "disciplines", "titles"] {
                assert!(collections.contains(&serde_json::json!(collection)), "{collection}");
            }
            // Title parents keep the discipline identifier type in every published body.
            assert!(document["components"]["schemas"]["directory.discipline.Id"].is_object());
            let text = serde_json::to_string(&document).expect("document text");
            assert!(text.contains(r##""disciplineId":{"$ref":"#/components/schemas/directory.discipline.Id"}"##));
            assert!(text.contains(r##""categoryId":{"$ref":"#/components/schemas/directory.category.Id"}"##));
            stop.send(()).expect("stop"); let (application, result) = task.await.expect("server task"); result.expect("shutdown"); drop(application); supplier.task.abort(); sms.stop().await;
        })).await.expect("bounded title tree HTTP acceptance");
    }));
}

#[test]
fn workforce_department_positions_replace_through_real_http() {
    let server = server();
    Runtime::new().expect("runtime").block_on(Box::pin(async {
        timeout(Duration::from_secs(90), Box::pin(async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let institution = issued_session_for(&base, &mut sms, "13900000000").await;
            let manager = issued_session_for(&base, &mut sms, "13800138000").await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let source = std::cell::Cell::new(1_u8);
            let request = |method: reqwest::Method, path: &str, actor: &(String, String), operation: Uuid| {
                let address = source.get(); source.set(address + 1);
                client.request(method, format!("{base}{path}")).header("operation-id", operation.to_string())
                    .header("session-id", &actor.0).header("cookie", &actor.1)
                    .header("origin", "https://example.com").header("x-real-ip", format!("203.0.172.{address}"))
            };
            let role = request(reqwest::Method::POST, "/api/v1/workforce/roles", &institution, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","name":"Roster manager","level":100,"permissions":["workforce.directory"]})).send().await.expect("directory-only role");
            assert_eq!(role.status(), 201);
            let role = role.json::<serde_json::Value>().await.expect("role")["role"].clone();
            let candidate = request(reqwest::Method::POST, "/api/v1/workforce/grants/candidates", &institution, Uuid::new_v4())
                .json(&serde_json::json!({"entry":"institution","mobile":"13800138000"})).send().await.expect("candidate").json::<serde_json::Value>().await.expect("confirmation");
            let admission = serde_json::json!({"entry":"institution","target":{
                "accountId":candidate["identity"]["accountId"],"personId":candidate["identity"]["personId"],"revisionId":candidate["identity"]["revisionId"],
                "mobile":"13800138000","assignmentId":candidate["assignmentId"],"suspensionId":candidate["suspensionId"],"grant":candidate["grant"]
            },"role":{"id":role["id"],"changeId":role["changeId"]}});
            assert_eq!(request(reqwest::Method::POST, "/api/v1/workforce/grants", &institution, Uuid::new_v4()).json(&admission).send().await.expect("admit manager").status(), 201);

            let group = request(reqwest::Method::POST, "/api/v1/workforce/directories/groups", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"name":"HTTP roster group","enabled":true,"index":0})).send().await.expect("group").json::<serde_json::Value>().await.expect("group")["item"].clone();
            let department = request(reqwest::Method::POST, "/api/v1/workforce/directories/departments", &manager, Uuid::new_v4())
                .json(&serde_json::json!({"name":"HTTP roster department","enabled":true,"index":0,"groupId":group["id"],"parentId":null})).send().await.expect("department").json::<serde_json::Value>().await.expect("department")["item"].clone();
            let mut positions = Vec::new();
            for name in ["HTTP roster lead", "HTTP roster nurse"] {
                tokio::time::sleep(Duration::from_millis(125)).await;
                positions.push(request(reqwest::Method::POST, "/api/v1/workforce/directories/positions", &manager, Uuid::new_v4())
                    .json(&serde_json::json!({"name":name,"enabled":true,"index":0})).send().await.expect("position").json::<serde_json::Value>().await.expect("position")["item"].clone());
            }
            let department_path = format!("/api/v1/workforce/directories/departments/{}", department["id"].as_str().expect("department id"));
            let path = format!("{department_path}/positions");
            assert_eq!(request(reqwest::Method::GET, &path, &institution, Uuid::new_v4()).send().await.expect("institution permission").status(), 403);
            let response = request(reqwest::Method::GET, &path, &manager, Uuid::new_v4()).send().await.expect("empty list");
            assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("empty"), serde_json::json!({"code":"ok","positions":[]}));
            let response = request(reqwest::Method::HEAD, &path, &manager, Uuid::new_v4()).send().await.expect("HEAD");
            assert_eq!(response.status(), 200); assert!(response.bytes().await.expect("empty HEAD").is_empty());

            let original = Uuid::new_v4();
            let body = serde_json::json!({"expected":[],"replacement":[{"positionId":positions[1]["id"],"enabled":true,"index":1},{"positionId":positions[0]["id"],"enabled":false,"index":0}]});
            let response = request(reqwest::Method::PUT, &path, &manager, original).json(&body).send().await.expect("replace");
            assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store"); assert!(!response.headers().contains_key("set-cookie"));
            let replaced = response.json::<serde_json::Value>().await.expect("list");
            let entries = replaced["positions"].as_array().expect("entries");
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0]["positionId"], positions[0]["id"]); assert_eq!(entries[0]["enabled"], false); assert_eq!(entries[0]["index"], 0);
            assert_eq!(entries[1]["positionId"], positions[1]["id"]); assert_eq!(entries[1]["index"], 1);
            assert_eq!(entries[0].as_object().expect("entry").len(), 4);
            let response = request(reqwest::Method::PUT, &path, &manager, original).json(&body).send().await.expect("replay");
            assert_eq!(response.status(), 200); assert_eq!(response.json::<serde_json::Value>().await.expect("original"), replaced);
            let response = request(reqwest::Method::GET, &path, &manager, Uuid::new_v4()).send().await.expect("configured read");
            assert_eq!(response.json::<serde_json::Value>().await.expect("configured")["positions"], replaced["positions"]);
            let mut different = body.clone(); different["replacement"][0]["index"] = serde_json::json!(7);
            for (operation, code) in [(original, "request_mismatch"), (Uuid::new_v4(), "data_changed")] {
                tokio::time::sleep(Duration::from_millis(125)).await;
                let response = request(reqwest::Method::PUT, &path, &manager, operation).json(&different).send().await.expect("conflict");
                assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":code}));
            }
            let missing = serde_json::json!({"expected":replaced["positions"],"replacement":[{"positionId":Uuid::new_v4(),"enabled":true,"index":0}]});
            let duplicate = serde_json::json!({"expected":replaced["positions"],"replacement":[{"positionId":positions[0]["id"],"enabled":true,"index":0},{"positionId":positions[0]["id"],"enabled":true,"index":1}]});
            let unknown = serde_json::json!({"expected":[],"replacement":[],"departmentId":department["id"]});
            let absent_path = format!("/api/v1/workforce/directories/departments/{}/positions", Uuid::new_v4());
            let group_path = format!("/api/v1/workforce/directories/departments/{}/positions", group["id"].as_str().expect("group id"));
            for (target, input, status, code) in [
                (path.as_str(), &missing, 404, "target_unavailable"),
                (path.as_str(), &duplicate, 422, "invalid_input"),
                (path.as_str(), &unknown, 422, "invalid_input"),
                (absent_path.as_str(), &body, 404, "target_unavailable"),
                (group_path.as_str(), &body, 404, "target_unavailable"),
            ] {
                tokio::time::sleep(Duration::from_millis(125)).await;
                let response = request(reqwest::Method::PUT, target, &manager, Uuid::new_v4()).json(input).send().await.expect("rejection");
                assert_eq!(response.status(), status, "{code}"); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":code}));
            }
            for (content_type, payload, status) in [("application/json", "{".to_owned(), 400), ("text/plain", "{}".to_owned(), 415), ("application/json", " ".repeat(8193), 413)] {
                tokio::time::sleep(Duration::from_millis(125)).await;
                let response = request(reqwest::Method::PUT, &path, &manager, Uuid::new_v4()).header("content-type", content_type).body(payload).send().await.expect("common JSON rejection");
                assert_eq!(response.status(), status); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":"invalid_input"}));
            }
            let unauthenticated = (Uuid::new_v4().to_string(), String::new());
            for method in [reqwest::Method::GET, reqwest::Method::PUT] {
                let response = request(method, "/api/v1/workforce/directories/departments/not-a-uuid/positions", &unauthenticated, Uuid::new_v4())
                    .header("content-type", "application/json").body(" ".repeat(8193)).send().await.expect("closed path before authentication and body");
                assert_eq!(response.status(), 400);
            }
            tokio::time::sleep(Duration::from_millis(125)).await;
            assert_eq!(request(reqwest::Method::PUT, &format!("{path}?extra=1"), &manager, Uuid::new_v4()).header("content-type", "application/json").body(" ".repeat(8193)).send().await.expect("query before body").status(), 400);
            assert_eq!(request(reqwest::Method::GET, &format!("{path}?page=1"), &manager, Uuid::new_v4()).send().await.expect("GET query").status(), 400);
            assert_eq!(request(reqwest::Method::GET, &path, &manager, Uuid::new_v4()).body("{}").send().await.expect("GET body").status(), 400);
            assert_eq!(request(reqwest::Method::PUT, &path, &manager, Uuid::new_v4()).header("origin", "https://wrong.invalid").json(&body).send().await.expect("Origin").status(), 403);
            assert_eq!(request(reqwest::Method::PUT, &path, &unauthenticated, Uuid::new_v4()).json(&body).send().await.expect("session").status(), 401);

            // Referenced items stay; the item routes beside this path still resolve.
            let position_path = format!("/api/v1/workforce/directories/positions/{}", positions[0]["id"].as_str().expect("position id"));
            for (target, expected) in [(position_path.as_str(), &positions[0]), (department_path.as_str(), &department)] {
                tokio::time::sleep(Duration::from_millis(125)).await;
                let response = request(reqwest::Method::DELETE, target, &manager, Uuid::new_v4()).json(&serde_json::json!({"expected":expected})).send().await.expect("referenced deletion");
                assert_eq!(response.status(), 409); assert_eq!(response.json::<serde_json::Value>().await.expect("code"), serde_json::json!({"code":"item_in_use"}));
            }
            let mut renamed = department.clone(); renamed["name"] = serde_json::json!("HTTP roster renamed");
            tokio::time::sleep(Duration::from_millis(125)).await;
            let response = request(reqwest::Method::PUT, &department_path, &manager, Uuid::new_v4()).json(&serde_json::json!({"expected":department,"replacement":renamed})).send().await.expect("department update beside the list path");
            assert_eq!(response.status(), 200);
            let response = request(reqwest::Method::GET, "/api/v1/workforce/directories/departments", &manager, Uuid::new_v4()).send().await.expect("collection beside the list path");
            assert_eq!(response.json::<serde_json::Value>().await.expect("departments")["items"], serde_json::json!([renamed]));

            let document = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json::<serde_json::Value>().await.expect("document");
            let operations = &document["paths"]["/api/v1/workforce/directories/departments/{id}/positions"];
            assert_eq!(operations["get"]["operationId"], "listWorkforceDepartmentPositions");
            assert!(operations["head"].is_object());
            let put = &operations["put"];
            assert_eq!(put["operationId"], "replaceWorkforceDepartmentPositions");
            assert_eq!(put["requestBody"]["content"]["application/json"]["schema"]["$ref"], "#/components/schemas/directory.roster.Replace");
            for status in ["200", "400", "401", "403", "404", "409", "413", "415", "422", "429", "503"] {
                assert!(put["responses"].get(status).is_some(), "{status}");
            }
            let entry = &document["components"]["schemas"]["directory.roster.Entry"];
            assert_eq!(entry["additionalProperties"], false);
            assert_eq!(entry["properties"]["positionId"]["$ref"], "#/components/schemas/directory.position.Id");
            stop.send(()).expect("stop"); let (application, result) = task.await.expect("server task"); result.expect("shutdown"); drop(application); supplier.task.abort(); sms.stop().await;
        })).await.expect("bounded department position HTTP acceptance");
    }));
}
