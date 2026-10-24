use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*, suspension::*};

#[test]
fn known_account_avatar_http_preserves_session_source_and_failure_boundaries() {
    use serde_json::{Value, json};

    const FAILED_OPERATION: &str = "00000000-0000-4000-9000-000000000106";
    if std::env::var_os("NUCLEUS_TEST_ACCOUNT_AVATAR_DIAGNOSTICS").is_none() {
        Runtime::new().expect("parent runtime").block_on(async {
            let child = tokio::process::Command::new(
                std::env::current_exe().expect("test executable"),
            )
            .args([
                "--exact",
                "avatar::known_account_avatar_http_preserves_session_source_and_failure_boundaries",
                "--nocapture",
            ])
            .env("NUCLEUS_TEST_ACCOUNT_AVATAR_DIAGNOSTICS", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("isolated diagnostics child");
            let output = timeout(Duration::from_secs(45), child.wait_with_output())
                .await
                .expect("child deadline")
                .expect("child result");
            let text = String::from_utf8(output.stderr).expect("JSON diagnostics");
            assert!(output.status.success(), "{text}");
            let events: Vec<Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).expect("JSON event"))
                .collect();
            let failures: Vec<_> = events
                .iter()
                .filter(|event| event["level"] == "ERROR")
                .collect();
            assert_eq!(failures.len(), 1, "exactly one final query failure");
            assert_eq!(failures[0]["fields"]["stage"], "account_avatar");
            assert_eq!(failures[0]["fields"]["operation_id"], FAILED_OPERATION);
            assert!(
                failures[0]["fields"]["error"]
                    .as_str()
                    .expect("safe cause")
                    .contains("Query")
            );
            for secret in [
                "13900000000",
                "13800000000",
                "__Host-nucleus-session-",
                "signature=",
                "https://avatars.example.com",
            ] {
                assert!(
                    !events.iter().any(|event| contains_text(event, secret)),
                    "private input or signed URL leaked"
                );
            }
        });
        return;
    }
    nucleus::diagnostics::init().expect("actual production log filter");
    isolated_child(|| {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let name = database(&server).await;
                let supplier = Supplier::echoing().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let application = Application::start(connections(&server, &name, "clerk"), keys(0),
                    services(8).expect("settings"),
                    process::Settings::new(Duration::from_secs(5)).expect("stop budget"), supplier.client(), message_at(sms.address(), sms.authority())).await.expect("startup");
                let (base, stop, task) = running(application).await;
                let manager = issued_session_from(&base, &mut sms, "13900000000", "203.0.155.1").await;
                let ordinary = issued_session_from(&base, &mut sms, "13800000000", "203.0.155.2").await;
                let client = http_client();
                let request = |actor: &(String, String), method: reqwest::Method, path: &str, source: u8| client.request(method, format!("{base}{path}"))
                    .header("operation-id", Uuid::new_v4().to_string()).header("session-id", &actor.0).header("cookie", &actor.1).header("x-real-ip", format!("203.0.155.{source}"));
                let observer = connections(&server, &name, "clerk").recover().await.expect("independent SQL reader");
                let work = Box::pin(async {
                    let response = request(&manager, reqwest::Method::GET, "/api/v1/accounts/current", 3).send().await.expect("current account");
                    assert_eq!(response.status(), 200);
                    let profile: Value = response.json().await.expect("self profile");
                    let id = profile["account"]["id"].as_str().expect("known target account");
                    let path = format!("/api/v1/accounts/{id}/avatar");
                    let before: String = observer.client().query_one("SELECT jsonb_build_array((SELECT jsonb_agg(to_jsonb(avatar) ORDER BY id) FROM identity.avatars avatar), (SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session))::text", &[]).await.expect("original facts").get(0);
                    let mut signed = String::new();
                    for (reader, source) in [(&ordinary, 4), (&manager, 5)] {
                        let response = request(reader, reqwest::Method::GET, &path, source).send().await.expect("known avatar");
                        assert_eq!(response.status(), 200);
                        assert_eq!(response.headers()["cache-control"], "no-store");
                        assert!(!response.headers().contains_key("set-cookie"));
                        let value: Value = response.json().await.expect("avatar JSON");
                        assert_eq!(value.as_object().expect("envelope").len(), 2);
                        assert_eq!(value["code"], "ok");
                        let avatar = &value["avatar"];
                        assert_eq!(avatar.as_object().expect("avatar fields").len(), 6);
                        assert_eq!(avatar["revision"], profile["account"]["avatar"]["revision"]);
                        assert_eq!(avatar["mediaType"], "image/webp"); assert_eq!(avatar["width"], 512); assert_eq!(avatar["height"], 512);
                        let url = url::Url::parse(avatar["href"].as_str().expect("signed link")).expect("URL");
                        assert_eq!(url.path(), "/api/v1/avatars/a92a8f686199a7388e2132dd61643a8df504b13379771e147c8157e06fb197cb.webp");
                        signed = format!("{}?{}", url.path(), url.query().expect("signature fields"));
                        let image = client.get(format!("{base}{signed}")).header("x-real-ip", format!("203.0.155.{}", source + 20)).send().await.expect("anonymous signed image");
                        assert_eq!(image.status(), 200); assert_eq!(image.headers()["content-type"], "image/webp");
                        assert_eq!(image.bytes().await.expect("complete image").as_ref(), include_bytes!("../../assets/avatars/0-cat.webp"));
                    }
                    let after: String = observer.client().query_one("SELECT jsonb_build_array((SELECT jsonb_agg(to_jsonb(avatar) ORDER BY id) FROM identity.avatars avatar), (SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session))::text", &[]).await.expect("read facts unchanged").get(0);
                    assert_eq!(before, after);
                    let response = request(&ordinary, reqwest::Method::HEAD, &path, 6).send().await.expect("HEAD");
                    assert_eq!(response.status(), 200); assert_eq!(response.headers()["cache-control"], "no-store");
                    assert!(response.bytes().await.expect("HEAD body").is_empty());
                    let response = request(&ordinary, reqwest::Method::GET, &format!("/api/v1/accounts/{}/avatar", Uuid::new_v4()), 7).send().await.expect("unknown account");
                    assert_eq!(response.status(), 404); assert_eq!(response.json::<Value>().await.expect("hidden outcome"), json!({"code":"target_unavailable"}));
                    for (builder, expected) in [
                        (request(&ordinary, reqwest::Method::GET, &path, 8).header("authorization", "Bearer ignored"), 400),
                        (request(&ordinary, reqwest::Method::GET, &path, 9).header("origin", "https://other.example.com"), 403),
                        (request(&ordinary, reqwest::Method::GET, &path, 10).header("sec-fetch-site", "cross-site"), 403),
                        (request(&ordinary, reqwest::Method::GET, &format!("{path}?extra=1"), 11), 400),
                        (request(&ordinary, reqwest::Method::GET, &path, 12).body("{}"), 400),
                        (client.get(format!("{base}/api/v1/accounts/invalid/avatar")).header("operation-id", Uuid::new_v4().to_string()).header("x-real-ip", "203.0.155.13"), 400),
                        (client.get(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string()).header("x-real-ip", "203.0.155.14"), 401),
                    ] {
                        assert_eq!(builder.send().await.expect("HTTP input and source boundary").status(), expected);
                    }
                    let plain = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("no default Origin client");
                    for (site, expected) in [(Some("same-origin"), 200), (None, 403)] {
                        let mut builder = plain.get(format!("{base}{path}")).header("operation-id", Uuid::new_v4().to_string()).header("session-id", &ordinary.0).header("cookie", &ordinary.1).header("x-real-ip", "203.0.155.15");
                        if let Some(site) = site { builder = builder.header("sec-fetch-site", site); }
                        assert_eq!(builder.send().await.expect("conditional same-origin read").status(), expected);
                    }
                    let owner = connections(&server, &name, "postgres").recover().await.expect("isolated permission fault");
                    owner.client().batch_execute("REVOKE SELECT ON identity.avatars FROM clerk").await.expect("fail only target metadata query");
                    let response = client.get(format!("{base}{path}")).header("operation-id", FAILED_OPERATION).header("session-id", &ordinary.0).header("cookie", &ordinary.1).header("x-real-ip", "203.0.155.16").send().await.expect("technical failure");
                    assert_eq!(response.status(), 503); assert_eq!(response.json::<Value>().await.expect("failure body"), json!({"code":"dependency_unavailable"}));
                    owner.client().batch_execute("GRANT SELECT ON identity.avatars TO clerk").await.expect("restore isolated permission");
                    owner.terminate().await.expect("owner ended");
                    assert_eq!(request(&ordinary, reqwest::Method::GET, &path, 17).send().await.expect("admission remains usable").status(), 200);
                    let candidate = account_candidate(&base, &manager, "13800000000").await;
                    let target_path = format!("/api/v1/accounts/{}/avatar", candidate["identity"]["accountId"].as_str().expect("ordinary account ID"));
                    suspend_account(&base, &manager, &candidate, "13800000000").await;
                    assert_eq!(request(&manager, reqwest::Method::GET, &target_path, 18).send().await.expect("paused target retains avatar").status(), 200);
                    assert_eq!(request(&ordinary, reqwest::Method::GET, &path, 19).send().await.expect("paused caller is invalid").status(), 401);
                    assert_eq!(client.get(format!("{base}{signed}")).header("x-real-ip", "203.0.155.27").send().await.expect("published link survives caller suspension").status(), 200);
                    let document: Value = client.get(format!("{base}/openapi/v1.json")).send().await.expect("OpenAPI").json().await.expect("schema");
                    let get = &document["paths"]["/api/v1/accounts/{id}/avatar"]["get"];
                    assert_eq!(get["operationId"], "accountAvatar");
                    assert!(get["parameters"].as_array().expect("parameters").iter().all(|parameter| parameter["name"] != "Origin"));
                    assert_eq!(document["components"]["schemas"]["account.Avatar"]["properties"]["avatar"]["$ref"], "#/components/schemas/avatar.Current");
                    assert_eq!(document["components"]["schemas"]["current.Account"]["properties"]["avatar"]["$ref"], "#/components/schemas/avatar.Current");
                    stop.send(()).expect("stop"); let (application, ended) = task.await.expect("server ended"); ended.expect("drained"); drop(application);
                });
                tokio::select! { biased; _=sms.request()=>panic!("avatar reads, management and drain never send SMS"), ()=work=>{} }
                assert_eq!(supplier.calls.load(Ordering::SeqCst), 2, "only the two original session issuances call verification");
                observer.terminate().await.expect("observer ended"); sms.stop().await;
            })).await.expect("bounded public avatar read lifecycle");
        }));
    });
}
