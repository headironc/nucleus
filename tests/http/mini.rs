use std::{cell::Cell, sync::atomic::Ordering, time::Duration};

use serde_json::{Value, json};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{
    application::*,
    database::*,
    supplier::{Mini, Supplier},
};

fn phone(code: &str) -> Value {
    json!({"secret":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","credential":{"type":"mobile","code":code}})
}

#[test]
fn mini_sessions_list_and_targeted_logout_preserve_the_initiator() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(35), async {
            let name = database(&server).await;
            let supplier = Supplier::new().await;
            let mini = Mini::start().await;
            let app = start_mini(&server, &name, &supplier, &mini).await;
            let (base, stop, task) = running(app).await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client");
            let sequence = Cell::new(1_u16);
            let request = |method, path: &str, operation: &str| {
                let address = sequence.get(); sequence.set(address + 1);
                client.request(method, format!("{base}/api/v1{path}")).header("operation-id", operation)
                    .header("x-real-ip", format!("203.0.{}.{}", address / 250, address % 250 + 1))
            };
            let response = request(reqwest::Method::POST, "/sessions", &Uuid::new_v4().to_string()).json(&phone("register")).send().await.expect("R1");
            assert_eq!(response.status(), 200);
            let registration = response.json::<Value>().await.expect("registration")["registration"].clone();
            let response = request(reqwest::Method::POST, "/registrations", &Uuid::new_v4().to_string()).json(&json!({"id":registration["id"],"token":registration["token"],"name":"隔离测试","idNumber":"00000020000229007X"})).send().await.expect("R2");
            assert_eq!(response.status(), 201);
            let actor = response.json::<Value>().await.expect("actor")["session"].clone();
            let response = request(reqwest::Method::POST, "/sessions", &Uuid::new_v4().to_string()).json(&phone("other-login")).send().await.expect("other login");
            assert_eq!(response.status(), 200);
            let target = response.json::<Value>().await.expect("target")["session"].clone();
            let authorization = format!("Bearer {}", actor["token"].as_str().expect("token"));
            let path = format!("/sessions/{}", target["id"].as_str().expect("id"));
            for path in ["/sessions".to_owned(), path.clone()] {
                let method = if path == "/sessions" { reqwest::Method::GET } else { reqwest::Method::DELETE };
                let response = request(method, &path, &Uuid::new_v4().to_string()).header("authorization", &authorization).header("session-id", actor["id"].as_str().expect("id")).send().await.expect("mixed proof rejected");
                assert_eq!(response.status(), 400); assert!(!response.headers().contains_key("set-cookie"));
            }
            let response = request(reqwest::Method::GET, "/sessions", &Uuid::new_v4().to_string()).header("authorization", &authorization).send().await.expect("list without Origin");
            assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store");
            let list = response.json::<Value>().await.expect("list");
            let sessions = list["sessions"].as_array().expect("sessions"); assert_eq!(sessions.len(), 2);
            for session in sessions {
                assert_eq!(session.as_object().expect("session").len(), 4);
                assert_eq!(session["source"], "mini");
                assert_eq!(session["current"], session["id"] == actor["id"]);
                chrono::DateTime::parse_from_rfc3339(session["createdAt"].as_str().expect("createdAt")).expect("timestamp");
            }
            let response = request(reqwest::Method::DELETE, &format!("/sessions/{}", actor["id"].as_str().expect("id")), &Uuid::new_v4().to_string()).header("authorization", &authorization).send().await.expect("current target");
            assert_eq!(response.status(), 409); assert_eq!(response.json::<Value>().await.expect("rejection")["code"], "current_session");
            let operation = Uuid::new_v4().to_string();
            let response = request(reqwest::Method::DELETE, &path, &operation).header("authorization", &authorization).body("{}").send().await.expect("nonempty body");
            assert_eq!(response.status(), 400);
            let response = request(reqwest::Method::DELETE, &path, &operation).header("authorization", &authorization).send().await.expect("target logout");
            assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<Value>().await.expect("receipt")["completion"]["count"], 1);
            let response = request(reqwest::Method::DELETE, &path, &Uuid::new_v4().to_string()).header("authorization", &authorization).send().await.expect("already revoked");
            assert_eq!(response.status(), 404); assert_eq!(response.json::<Value>().await.expect("hidden")["code"], "resource_not_found");
            let response = request(reqwest::Method::GET, "/sessions", &Uuid::new_v4().to_string()).header("authorization", &authorization).send().await.expect("initiator retained");
            assert_eq!(response.status(), 200); assert_eq!(response.json::<Value>().await.expect("remaining")["sessions"].as_array().expect("sessions").len(), 1);
            let response = request(reqwest::Method::GET, &format!("/revocations?operation_id={operation}"), &Uuid::new_v4().to_string()).header("authorization", &authorization).send().await.expect("permanent original");
            assert_eq!(response.status(), 200);
            let original = response.json::<Value>().await.expect("original");
            assert_eq!(original["original"], json!({"scope":"other","initiatorId":actor["id"]}));
            assert_eq!(original["revocations"][0]["id"], target["id"]);
            assert_eq!(mini.calls.load(Ordering::SeqCst), 2);
            stop.send(()).expect("stop"); let (app, result) = task.await.expect("server"); result.expect("drained"); drop(app);
            drop(mini);
        }).await.expect("mini session management budget");
    });
}

#[test]
fn mini_http_registration_current_identity_and_logout_use_the_original_bearer_source() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(40), async {
            let name = database(&server).await;
            let supplier = Supplier::new().await;
            let mini = Mini::start().await;
            let app = start_mini(&server, &name, &supplier, &mini).await;
            let (base, stop, task) = running(app).await;
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("HTTP client without browser Origin");
            let sequence = Cell::new(1_u16);
            let request = |method, path: &str, operation: &str| {
                let address = sequence.get(); sequence.set(address + 1);
                client.request(method, format!("{base}/api/v1{path}")).header("operation-id", operation)
                    .header("x-real-ip", format!("203.0.{}.{}", address / 250, address % 250 + 1))
            };
            let r1 = Uuid::new_v4().to_string();
            let response = request(reqwest::Method::POST, "/sessions", &r1).json(&phone("registration-code")).send().await.expect("phone R1");
            assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie"));
            let registration = response.json::<Value>().await.expect("registration")["registration"].clone();
            let body = json!({"id":registration["id"],"token":registration["token"],"name":"隔离测试","idNumber":"00000020000229007X"});
            let r2 = Uuid::new_v4().to_string();
            let response = request(reqwest::Method::POST, "/registrations", &r2).json(&body).send().await.expect("mini R2 without Origin");
            assert_eq!(response.status(), 201); assert!(!response.headers().contains_key("set-cookie"));
            let original = response.json::<Value>().await.expect("mini session")["session"].clone();
            assert_eq!(original.as_object().expect("session object").len(), 3);
            let authorization = format!("Bearer {}", original["token"].as_str().expect("opaque token"));
            let response = request(reqwest::Method::POST, "/registrations", &r2).json(&body).send().await.expect("R2 replay");
            assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<Value>().await.expect("original receipt")["session"], original);
            let response = request(reqwest::Method::GET, "/accounts/current", &Uuid::new_v4().to_string()).header("authorization", &authorization).send().await.expect("current mini identity");
            assert_eq!(response.status(), 200); assert_eq!(response.json::<Value>().await.expect("profile")["account"]["name"], "隔离测试");
            let cookie = format!("__Host-nucleus-session-{}={}", original["id"].as_str().expect("id"), original["token"].as_str().expect("token"));
            for conflict in ["session-id", "cookie", "authorization"] {
                let value = match conflict { "session-id" => original["id"].as_str().expect("id"), "cookie" => &cookie, _ => &authorization };
                let response = request(reqwest::Method::GET, "/accounts/current", &Uuid::new_v4().to_string()).header("authorization", &authorization).header(conflict, value).send().await.expect("conflicting proof");
                assert_eq!(response.status(), 400, "{conflict}"); assert!(!response.headers().contains_key("set-cookie"));
            }
            let response = request(reqwest::Method::GET, "/accounts/current", &Uuid::new_v4().to_string()).header("session-id", original["id"].as_str().expect("id")).header("cookie", &cookie).header("origin", "https://example.com").send().await.expect("mini token presented as cookie");
            assert_eq!(response.status(), 401);
            let response = request(reqwest::Method::GET, "/accounts/current", &Uuid::new_v4().to_string()).header("authorization", &authorization).send().await.expect("unrelated admission remains open");
            assert_eq!(response.status(), 200);
            let logout = Uuid::new_v4().to_string();
            let response = request(reqwest::Method::DELETE, "/sessions", &logout).header("authorization", &authorization).send().await.expect("mini logout without Origin");
            assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<Value>().await.expect("logout")["completion"]["count"], 1);
            let response = request(reqwest::Method::DELETE, "/sessions", &logout).header("authorization", &authorization).send().await.expect("revoked proof");
            assert_eq!(response.status(), 401); assert!(!response.headers().contains_key("set-cookie"));
            let response = request(reqwest::Method::POST, "/sessions", &Uuid::new_v4().to_string()).json(&phone("new-login-code")).send().await.expect("new same-account session");
            assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie"));
            let next = response.json::<Value>().await.expect("next session")["session"].clone();
            let authorization = format!("Bearer {}", next["token"].as_str().expect("next token"));
            let response = request(reqwest::Method::GET, &format!("/revocations?operation_id={logout}"), &Uuid::new_v4().to_string()).header("authorization", &authorization).send().await.expect("original logout read");
            assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie"));
            let result = response.json::<Value>().await.expect("original logout");
            assert_eq!(result["original"], json!({"scope":"all","initiatorId":original["id"]}));
            assert_eq!(result["revocations"][0]["id"], original["id"]);
            let response = request(reqwest::Method::DELETE, "/sessions", &logout).header("authorization", &authorization).send().await.expect("old operation is not a new logout");
            assert_eq!(response.status(), 409);
            assert_eq!(mini.calls.load(Ordering::SeqCst), 2); assert_eq!(mini.tokens.load(Ordering::SeqCst), 1);
            let response = request(reqwest::Method::POST, "/sessions", &Uuid::new_v4().to_string()).json(&phone("parallel-login-code")).send().await.expect("another live mini session");
            assert_eq!(response.status(), 200);
            let parallel = response.json::<Value>().await.expect("parallel session")["session"].clone();
            let current_logout = Uuid::new_v4().to_string();
            let response = request(reqwest::Method::DELETE, "/sessions/current", &current_logout).header("authorization", &authorization).send().await.expect("current mini logout without Origin");
            assert_eq!(response.status(), 200); assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(response.json::<Value>().await.expect("current logout")["completion"]["count"], 1);
            let response = request(reqwest::Method::GET, "/accounts/current", &Uuid::new_v4().to_string()).header("authorization", &authorization).send().await.expect("selected proof is revoked");
            assert_eq!(response.status(), 401);
            let parallel_authorization = format!("Bearer {}", parallel["token"].as_str().expect("parallel token"));
            let response = request(reqwest::Method::GET, "/accounts/current", &Uuid::new_v4().to_string()).header("authorization", &parallel_authorization).send().await.expect("other mini session remains usable");
            assert_eq!(response.status(), 200);
            let response = request(reqwest::Method::GET, &format!("/revocations?operation_id={current_logout}"), &Uuid::new_v4().to_string()).header("authorization", &parallel_authorization).send().await.expect("original current logout");
            assert_eq!(response.status(), 200);
            assert_eq!(response.json::<Value>().await.expect("current original")["original"], json!({"scope":"current","initiatorId":next["id"]}));
            for (code, status, application_code) in [("invalid", 401, "authentication_invalid"), ("unsupported", 403, "mobile_unsupported")] {
                let response = request(reqwest::Method::POST, "/sessions", &Uuid::new_v4().to_string()).json(&phone(code)).send().await.expect("phone refusal without Origin");
                assert_eq!(response.status(), status);
                assert!(!response.headers().contains_key("set-cookie"));
                assert_eq!(response.json::<Value>().await.expect("safe refusal")["code"], application_code);
            }
            stop.send(()).expect("stop"); let (mut app, result) = task.await.expect("server"); result.expect("serve result"); app.stop().await.expect("drained application");
        }).await.expect("HTTP mini acceptance budget");
    });
}

#[test]
fn an_unconfigured_mini_capability_fails_without_calling_any_supplier() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(25), async {
            let name = database(&server).await;
            let supplier = Supplier::new().await;
            let app = start(&server, &name, 0, 8, &supplier).await;
            let (base, stop, task) = running(app).await;
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("HTTP client");
            let response = client
                .post(format!("{base}/api/v1/sessions"))
                .header("operation-id", Uuid::new_v4().to_string())
                .header("x-real-ip", "203.0.113.86")
                .json(&phone("unconfigured-code"))
                .send()
                .await
                .expect("unconfigured mobile R1");
            assert_eq!(response.status(), 503);
            assert!(!response.headers().contains_key("set-cookie"));
            assert_eq!(
                response
                    .json::<Value>()
                    .await
                    .expect("safe unavailable response")["code"],
                "dependency_unavailable"
            );
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 0);
            let observer = connections(&server, &name, "clerk")
                .recover()
                .await
                .expect("isolated observer");
            assert_eq!(
                observer
                    .client()
                    .query_one("SELECT count(*) FROM identity.exchanges", &[])
                    .await
                    .expect("no occupancy")
                    .get::<_, i64>(0),
                0
            );
            observer.terminate().await.expect("observer ended");
            stop.send(()).expect("stop");
            let (mut app, result) = task.await.expect("server");
            result.expect("serve result");
            app.stop().await.expect("drained");
        })
        .await
        .expect("optional mini capability budget");
    });
}
